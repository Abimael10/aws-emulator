// SPDX-License-Identifier: CC0-1.0

//! The resource-based policy a SHARED billing view carries, end to end over the
//! awsJson1_0 wire.
//!
//! One rule: a custom billing view carries the policy document the account's
//! resource shares attach to it — `GetResourcePolicy` reports the statements
//! those shares instantiate on the view, and answers 404
//! `ResourceNotFoundException` while nothing has attached one. This service's
//! model declares no operation that attaches a policy (there is no
//! `PutResourcePolicy`), so the path that attaches one is the account SHARING
//! the view, which is AWS Resource Access Manager's `CreateResourceShare` — and
//! that is the path every case here drives, over one engine both services are
//! mounted on, the way `core/router` mounts them in production.
//!
//! The tables span what the rule generalises over: how many shares share the
//! view, how many principals each share names and in which form, whether a
//! principal is one that needs a grant at all, which billing-view type owns the
//! ARN, and how the sharing is taken away again.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{
    call, error_code, set_organization_management_account, ACCOUNT_ID, PRIMARY_ARN,
};

/// An account outside the one this emulator serves: a principal that needs a
/// resource-based policy to reach a view shared with it.
const OTHER_ACCOUNT: &str = "444455556666";

/// A second one, so a case can share with more than one identity.
const THIRD_ACCOUNT: &str = "777788889999";

/// The policy language version every statement of a resource policy is written
/// for (`Version`, as the shares instantiate the document).
const POLICY_VERSION: &str = "2012-10-17";

/// One router over ONE shared engine carrying both the billing service, whose
/// `GetResourcePolicy` is under test, and the resource sharing service, whose
/// shares are the only thing that attaches a policy to a billing view.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_billing_awsbilling::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ram_amazonresourcesharing::EmulatedService::new(Arc::clone(&engine)),
    );
    // `CreateBillingView` is reserved to the organization's management account,
    // which is AWS Organizations' fact — the world in which a custom view exists
    // to be shared at all.
    set_organization_management_account(&engine, Some(ACCOUNT_ID));
    (router, engine)
}

/// Send one restJson1 call to the resource sharing service and return its HTTP
/// status and decoded body.
///
/// The `Authorization` credential scope names `ram`, which is how the router
/// picks that service out of the same engine the billing calls reach.
async fn share_call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "ram.us-east-1.amazonaws.com")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/ram/aws4_request, \
             SignedHeaders=host, Signature=0",
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        }
        None => bytes::Bytes::new(),
    };
    let response = router
        .dispatch(builder.body(payload).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Create one CUSTOM billing view drawing from the account's PRIMARY view and
/// return its ARN.
async fn create_view(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = call(
        router,
        "CreateBillingView",
        &json!({ "name": name, "sourceViews": [PRIMARY_ARN] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBillingView to answer 200, got {status} {body:?}"
    );
    body["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateBillingView answered no arn: {body:?}"))
        .to_string()
}

/// Share `resource_arn` with `principals` and return the ARN of the share that
/// now shares it.
async fn share_resource(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    resource_arn: &str,
    principals: &[&str],
) -> String {
    let (status, body) = share_call(
        router,
        "POST",
        "/createresourceshare",
        Some(&json!({
            "name": name,
            "resourceArns": [resource_arn],
            "principals": principals,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateResourceShare to answer 200 for {resource_arn}, \
         got {status} {body:?}"
    );
    body["resourceShare"]["resourceShareArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the created share reported no ARN: {body:?}"))
        .to_string()
}

/// The `GetResourcePolicy` answer for `resource_arn`: its HTTP status and body.
async fn get_policy(router: &MultiServiceRouter, resource_arn: &str) -> (u16, Value) {
    call(
        router,
        "GetResourcePolicy",
        &json!({ "resourceArn": resource_arn }),
    )
    .await
}

/// The statements of the policy document a 200 answer carries, as JSON values.
fn statements(case: &str, body: &Value) -> Vec<Value> {
    let document = body["policy"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carried no policy: {body:?}"));
    let parsed: Value = serde_json::from_str(document)
        .unwrap_or_else(|_| panic!("case {case:?}: the policy is not JSON: {document:?}"));
    assert_eq!(
        parsed["Version"].as_str(),
        Some(POLICY_VERSION),
        "case {case:?}: expected the policy language version, got {parsed:?}"
    );
    parsed["Statement"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the policy carries no Statement list: {parsed:?}"))
}

/// Every identity the statements of a policy document grant to, in ARN order so
/// a case names the set the shares grant to rather than the order the shares
/// happen to be reported in.
fn granted_principals(case: &str, body: &Value) -> Vec<String> {
    let mut granted: Vec<String> = statements(case, body)
        .iter()
        .flat_map(|statement| match &statement["Principal"]["AWS"] {
            Value::String(one) => vec![one.clone()],
            Value::Array(many) => many
                .iter()
                .map(|entry| {
                    entry
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: a principal must be a string, got {entry:?}")
                        })
                        .to_string()
                })
                .collect(),
            other => panic!("case {case:?}: a statement granted nobody, got {other:?}"),
        })
        .collect();
    granted.sort();
    granted
}

/// One row of the sharing table: how the view is shared, and which identities
/// the policy the read reports must then grant — empty for a view that carries
/// no policy at all.
struct SharingRow {
    case: &'static str,
    /// One entry per share that shares the view, each naming its principals.
    shares: Vec<Vec<&'static str>>,
    granted: Vec<&'static str>,
}

fn sharing_rows() -> Vec<SharingRow> {
    vec![
        SharingRow {
            case: "one share, one account outside the owner's",
            shares: vec![vec![OTHER_ACCOUNT]],
            granted: vec![OTHER_ACCOUNT],
        },
        SharingRow {
            case: "one share, a principal written as an ARN",
            shares: vec![vec!["arn:aws:iam::444455556666:root"]],
            granted: vec!["arn:aws:iam::444455556666:root"],
        },
        SharingRow {
            case: "one share, two accounts",
            shares: vec![vec![OTHER_ACCOUNT, THIRD_ACCOUNT]],
            granted: vec![OTHER_ACCOUNT, THIRD_ACCOUNT],
        },
        SharingRow {
            case: "two shares, one account each",
            shares: vec![vec![OTHER_ACCOUNT], vec![THIRD_ACCOUNT]],
            granted: vec![OTHER_ACCOUNT, THIRD_ACCOUNT],
        },
        SharingRow {
            case: "three shares, one of them with two accounts",
            shares: vec![
                vec![OTHER_ACCOUNT],
                vec![THIRD_ACCOUNT, "111122223333"],
                vec!["555566667777"],
            ],
            granted: vec![
                OTHER_ACCOUNT,
                THIRD_ACCOUNT,
                "111122223333",
                "555566667777",
            ],
        },
        SharingRow {
            // An identity inside the owner's own account reaches the view without
            // a resource-based policy, so sharing with it attaches none.
            case: "one share, the owner's own account",
            shares: vec![vec![ACCOUNT_ID]],
            granted: vec![],
        },
        SharingRow {
            // A share that names no principal shares the view with nobody.
            case: "one share, no principal at all",
            shares: vec![vec![]],
            granted: vec![],
        },
    ]
}

#[tokio::test]
async fn the_policy_a_view_carries_is_the_one_its_shares_attach() {
    let (router, _engine) = emulator();

    for (index, row) in sharing_rows().into_iter().enumerate() {
        let case = row.case;
        let view = create_view(&router, case, &format!("shared-view-{index}")).await;

        // Before anything shares it, the view carries no policy at all.
        let (status, body) = get_policy(&router, &view).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: a view no share shares carries no policy, got {status} {body:?}"
        );

        for (share_index, principals) in row.shares.iter().enumerate() {
            share_resource(
                &router,
                case,
                &format!("share-{index}-{share_index}"),
                &view,
                principals,
            )
            .await;
        }

        let (status, body) = get_policy(&router, &view).await;
        if row.granted.is_empty() {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: shares that grant nothing attach no policy, got {status} {body:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the shared view's policy, got {status} {body:?}"
        );
        assert_eq!(
            body["resourceArn"].as_str(),
            Some(view.as_str()),
            "case {case:?}: the answer must name the resource it answered for, got {body:?}"
        );
        // One document, holding one statement per share that grants something.
        let reported = statements(case, &body);
        assert_eq!(
            reported.len(),
            row.shares
                .iter()
                .filter(|principals| !principals.is_empty())
                .count(),
            "case {case:?}: expected one statement per share that grants something, got {reported:?}"
        );
        for statement in &reported {
            assert_eq!(
                statement["Resource"].as_str(),
                Some(view.as_str()),
                "case {case:?}: every statement is about the view it is attached to, got {statement:?}"
            );
            let action = statement["Action"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a statement allowed nothing: {statement:?}"));
            assert!(
                action.starts_with("billing:"),
                "case {case:?}: expected an action of the shared resource's own service, \
                 got {action:?}"
            );
            assert_eq!(
                statement["Effect"].as_str(),
                Some("Allow"),
                "case {case:?}: a shared resource's policy grants, got {statement:?}"
            );
        }
        let mut expected: Vec<String> = row.granted.iter().map(|one| one.to_string()).collect();
        expected.sort();
        assert_eq!(
            granted_principals(case, &body),
            expected,
            "case {case:?}: expected the identities the shares name, got {body:?}"
        );
    }
}

/// One row of the teardown table: how the sharing that attached the policy is
/// taken away again.
struct TeardownRow {
    case: &'static str,
    /// The call that stops the share sharing the view: method, URI and body,
    /// built from the share's ARN and the view's.
    request: fn(&str, &str) -> (&'static str, String, Option<Value>),
}

fn teardown_rows() -> Vec<TeardownRow> {
    vec![
        TeardownRow {
            case: "the resource is disassociated from the share",
            request: |share_arn, view_arn| {
                (
                    "POST",
                    "/disassociateresourceshare".to_string(),
                    Some(json!({
                        "resourceShareArn": share_arn,
                        "resourceArns": [view_arn],
                    })),
                )
            },
        },
        TeardownRow {
            case: "the principal is disassociated from the share",
            request: |share_arn, _| {
                (
                    "POST",
                    "/disassociateresourceshare".to_string(),
                    Some(json!({
                        "resourceShareArn": share_arn,
                        "principals": [OTHER_ACCOUNT],
                    })),
                )
            },
        },
        TeardownRow {
            case: "the share itself is deleted",
            request: |share_arn, _| {
                (
                    "DELETE",
                    format!("/deleteresourceshare?resourceShareArn={share_arn}"),
                    None,
                )
            },
        },
    ]
}

#[tokio::test]
async fn the_policy_follows_the_shares_the_view_has_now() {
    let (router, _engine) = emulator();

    for (index, row) in teardown_rows().into_iter().enumerate() {
        let case = row.case;
        let view = create_view(&router, case, &format!("torn-down-view-{index}")).await;
        let share = share_resource(
            &router,
            case,
            &format!("torn-down-share-{index}"),
            &view,
            &[OTHER_ACCOUNT],
        )
        .await;

        let (status, body) = get_policy(&router, &view).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the shared view must carry a policy first, got {status} {body:?}"
        );

        let (method, uri, request) = (row.request)(&share, &view);
        let (status, body) = share_call(&router, method, &uri, request.as_ref()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the teardown to answer 200, got {status} {body:?}"
        );

        // The view is not shared any more, so there is no document attached to
        // it: the read answers its modelled 404 again.
        let (status, body) = get_policy(&router, &view).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: a view nothing shares carries no policy, got {status} {body:?}"
        );

        // Sharing it again attaches a policy again: the read reports what the
        // shares hold now, never what they once held.
        share_resource(
            &router,
            case,
            &format!("reshared-{index}"),
            &view,
            &[THIRD_ACCOUNT],
        )
        .await;
        let (status, body) = get_policy(&router, &view).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a view shared again carries a policy again, got {status} {body:?}"
        );
        assert_eq!(
            granted_principals(case, &body),
            vec![THIRD_ACCOUNT.to_string()],
            "case {case:?}: expected the identity the new share names, got {body:?}"
        );
    }
}

#[tokio::test]
async fn a_shared_arn_that_is_no_custom_view_of_this_account_carries_no_policy() {
    let (router, _engine) = emulator();

    // Each row is an ARN a share CAN hold but whose policy is not a custom view
    // of this account's: the managed PRIMARY view, whose sharing is the
    // service's own, and an ARN no view is stored under.
    let rows = [
        ("the managed PRIMARY view", PRIMARY_ARN.to_string()),
        (
            "an ARN no billing view answers to",
            format!("arn:aws:billing::{ACCOUNT_ID}:billingview/never-created"),
        ),
    ];

    for (index, (case, arn)) in rows.iter().enumerate() {
        share_resource(
            &router,
            case,
            &format!("other-share-{index}"),
            arn,
            &[OTHER_ACCOUNT],
        )
        .await;
        let (status, body) = get_policy(&router, arn).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: only a custom billing view of this account reports a policy, \
             got {status} {body:?}"
        );
    }

    // The control: the very same share of a CUSTOM view of this account does
    // report one, so the refusals above are about what the ARN names.
    let view = create_view(&router, "the control", "custom-view").await;
    share_resource(
        &router,
        "the control",
        "control-share",
        &view,
        &[OTHER_ACCOUNT],
    )
    .await;
    let (status, body) = get_policy(&router, &view).await;
    assert_eq!(
        status, 200,
        "case \"the control\": a custom view of this account reports its policy, \
         got {status} {body:?}"
    );
}

#[tokio::test]
async fn reading_the_policy_changes_nothing_a_later_caller_sees() {
    let (router, _engine) = emulator();
    let view = create_view(&router, "repeated reads", "read-twice").await;
    share_resource(
        &router,
        "repeated reads",
        "read-twice-share",
        &view,
        &[OTHER_ACCOUNT],
    )
    .await;

    let first = get_policy(&router, &view).await;
    let second = get_policy(&router, &view).await;
    assert_eq!(
        first, second,
        "case \"repeated reads\": two identical reads must answer the same, got {first:?} \
         then {second:?}"
    );
    assert_eq!(
        first.0, 200,
        "case \"repeated reads\": expected the shared view's policy, got {first:?}"
    );

    // What the read is derived from is untouched: the share still shares the
    // view and still names the identity it was created with.
    let (status, body) = share_call(
        &router,
        "POST",
        "/getresourceshareassociations",
        Some(&json!({ "associationType": "RESOURCE", "resourceArn": view })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"repeated reads\": reading the associations back expected 200, got {status} \
         {body:?}"
    );
    let entities: Vec<String> = body["resourceShareAssociations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|association| association["associatedEntity"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        entities,
        vec![view.clone()],
        "case \"repeated reads\": the share must still share exactly the view, got {body:?}"
    );
}

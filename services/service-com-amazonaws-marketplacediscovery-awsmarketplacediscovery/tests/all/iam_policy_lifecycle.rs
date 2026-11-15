// SPDX-License-Identifier: CC0-1.0

//! WHEN a change to a principal's identity-based policies takes effect on this service, and
//! what an explicit `Deny` overrides — driven over the real wire path, with the principal,
//! its long-term access key and its policy documents all made through IAM's OWN operations
//! on the same shared engine.
//!
//! THE RULE UNDER TEST: every request is decided against the documents in force on its
//! principal AT THE INSTANT IT ARRIVES, aggregated over ALL of that principal's
//! identity-based policies — so a freshly created principal carrying none falls to IAM's
//! default deny, a grant written after it lets the identical call through, an explicit
//! `Deny` in ANY of its documents overrides every `Allow` in the others whatever document
//! carries it, and taking that `Deny` off again restores the grant. No decision is cached
//! and none is taken once per principal: the answer to two identical calls differs when the
//! policies between them changed.
//!
//! `tests/all/iam_authorization.rs` is the table for WHICH document decides WHICH operation
//! at one instant; this suite is the table for the SEQUENCE — a put and a delete between two
//! identical calls — and for how the overriding `Deny` is written. Its awsQuery harness for
//! IAM is the one piece neither suite had: the calls here are `iam:CreateUser`,
//! `iam:CreateAccessKey`, `iam:PutUserPolicy` and `iam:DeleteUserPolicy`, so the documents
//! this service reads are the ones a client wrote and the principal a refusal blames is the
//! one IAM minted the signing key for.
//!
//! The rows span what the rule generalises over: the operation asked for (a read of each
//! record type the catalog addresses by id, and a search that addresses the catalog itself),
//! the step of the lifecycle (no policy, a grant, a grant plus a `Deny`, the `Deny` removed),
//! and how the `Deny` is written (exactly the action, every action of this service, every
//! action at all, scoped to the record the request addresses, scoped to another catalog's
//! records, naming another action of this service).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::iam_authorization::{
    call_as, catalog_records, operations, resource_of, Answer, Operation, Refusal, ACTION_PREFIX,
};

/// IAM's awsQuery API version, which every call below names (`iam:Version`).
const IAM_VERSION: &str = "2010-05-08";

/// The Region every request's SigV4 credential scope names.
const REGION: &str = "us-east-1";

/// The IAM user every case drives its calls as. One name is enough because every case runs
/// against an engine of its own, where nothing else holds it.
const USER_NAME: &str = "catalog-reader";

/// The inline policy a case writes its grant under, and the SECOND inline policy it writes
/// the overriding `Deny` under — two documents on one principal, which is what makes the
/// `Deny` a fact about a document other than the one that granted the action.
const GRANT_POLICY: &str = "catalog-read-grant";
const GUARD_POLICY: &str = "catalog-read-guard";

/// The operations this suite drives the lifecycle over: a read of each record type the
/// catalog addresses by id, and the search that addresses the catalog itself rather than any
/// record — the shapes of resource the decision is taken against.
const UNDER_TEST: &[&str] = &["GetListing", "GetOffer", "GetProduct", "SearchListings"];

/// The emulator these cases drive: this service mounted beside IAM — which holds the user,
/// the access key and the policy documents — and STS, which is where the assembled emulator
/// resolves a request's access key id FIRST, all over ONE shared engine. A long-term key is
/// a key STS minted no session for, so it falls through to the IAM user that owns it; mounting
/// STS is what makes the case take that whole path rather than a short one.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing is signed here: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One awsQuery call to IAM, answered with the HTTP status and the XML body. Unsigned,
    /// so it speaks for the account's own published credential — the caller that may
    /// administer its own identities.
    async fn iam(&self, action: &str, params: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version={IAM_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = ::http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260205/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = String::from_utf8(response.body().to_vec()).expect("a UTF-8 response body");
        (status, body)
    }

    /// Create the IAM user and mint it a long-term access key, with NO policy attached —
    /// the position a freshly created IAM principal is in. Answers the ARN IAM holds the
    /// user under (what a refusal names) and the access key id to sign this service's calls
    /// with, both read back out of IAM's own answers so neither is written out here.
    async fn given_user_with_no_policy(&self, case: &str) -> (String, String) {
        let (status, created) = self.iam("CreateUser", &[("UserName", USER_NAME)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateUser must succeed, got {status}: {created}"
        );
        let user_arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateUser answers an Arn, got {created}"));
        let (status, key) = self
            .iam("CreateAccessKey", &[("UserName", USER_NAME)])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateAccessKey must succeed, got {status}: {key}"
        );
        let access_key_id = xml_element(&key, "AccessKeyId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateAccessKey answers an id, got {key}"));
        (user_arn, access_key_id)
    }

    /// Write `document` on the user as the inline policy `policy_name`, replacing whatever
    /// that name carried — what `iam:PutUserPolicy` does.
    async fn put_user_policy(&self, case: &str, policy_name: &str, document: &Value) {
        let (status, put) = self
            .iam(
                "PutUserPolicy",
                &[
                    ("UserName", USER_NAME),
                    ("PolicyName", policy_name),
                    ("PolicyDocument", &document.to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutUserPolicy {policy_name:?} must succeed, got {status}: {put}"
        );
    }

    /// Take the inline policy `policy_name` off the user — what `iam:DeleteUserPolicy` does.
    async fn delete_user_policy(&self, case: &str, policy_name: &str) {
        let (status, deleted) = self
            .iam(
                "DeleteUserPolicy",
                &[("UserName", USER_NAME), ("PolicyName", policy_name)],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteUserPolicy {policy_name:?} must succeed, got {status}: \
             {deleted}"
        );
    }
}

/// A one-statement identity-based policy document: `effect` on `action` over `resource`.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }],
    })
}

/// Assert that `answer` is the modelled authorization refusal, worded the way IAM words
/// `refusal`: HTTP 403, the `AccessDeniedException` error code, and the sentence naming the
/// principal, the action and the resource the request addressed.
fn assert_refused(
    case: &str,
    answer: &Answer,
    principal: &str,
    action: &str,
    resource: &str,
    refusal: Refusal,
) {
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected HTTP 403, got {} ({:?})",
        answer.status, answer.body
    );
    assert!(
        answer.refused(),
        "case {case:?}: expected an AccessDeniedException, got {:?} ({:?})",
        answer.error_code,
        answer.body
    );
    assert_eq!(
        answer.message(),
        format!(
            "User: {principal} is not authorized to perform: {action} on resource: {resource} {}",
            refusal.reason(action)
        ),
        "case {case:?}: expected IAM's wording, got {:?}",
        answer.body
    );
}

/// Assert that `answer` is the operation's own answer for the record the request named: the
/// request got past the decision and was served.
fn assert_served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: an allowed request must be served, got {} ({:?})",
        answer.status, answer.body
    );
}

/// The operations [`UNDER_TEST`] names, each with a request of its own and the record its
/// decision is taken against.
fn under_test(listing_id: &str, product_id: &str, offer_id: &str) -> Vec<Operation> {
    let rows: Vec<Operation> = operations(listing_id, product_id, offer_id)
        .into_iter()
        .filter(|row| UNDER_TEST.contains(&row.name))
        .collect();
    assert_eq!(
        rows.len(),
        UNDER_TEST.len(),
        "the suite names {} operations this service serves, found {}",
        UNDER_TEST.len(),
        rows.len()
    );
    rows
}

/// THE RULE: the decision is taken against the documents in force when the request arrives.
/// The same call is refused by default deny while the principal carries nothing, served once
/// a grant is written, refused again — as an explicit deny — while a SECOND document carries
/// a `Deny` beside that grant, and served again once that document is deleted.
#[tokio::test]
async fn a_policy_written_between_two_identical_calls_decides_the_second() {
    let probe = Emulator::new();
    let (catalog, listing_id, product_id, offer_id) = catalog_records(&probe.router).await;

    for operation in under_test(&listing_id, &product_id, &offer_id) {
        let action = format!("{ACTION_PREFIX}:{}", operation.name);
        let resource = resource_of(&catalog, &operation.record);
        // One engine per operation, so the user this case creates carries only what this
        // case writes on it.
        let emulator = Emulator::new();
        let case = format!("{} with no policy at all", operation.name);
        let (user_arn, access_key_id) = emulator.given_user_with_no_policy(&case).await;

        // 1. A freshly created principal carrying no policy at all: IAM's default deny.
        let answer = call_as(
            &emulator.router,
            &access_key_id,
            operation.route,
            &operation.body,
        )
        .await;
        assert_refused(
            &case,
            &answer,
            &user_arn,
            &action,
            &resource,
            Refusal::DefaultDeny,
        );

        // 2. A grant written on it, and the IDENTICAL call made again.
        let case = format!("{} once a grant is written", operation.name);
        emulator
            .put_user_policy(&case, GRANT_POLICY, &statement("Allow", &action, "*"))
            .await;
        let answer = call_as(
            &emulator.router,
            &access_key_id,
            operation.route,
            &operation.body,
        )
        .await;
        assert_served(&case, &answer);

        // 3. A SECOND document carrying a `Deny` of the same action beside that grant.
        let case = format!("{} with a Deny in a second policy", operation.name);
        emulator
            .put_user_policy(&case, GUARD_POLICY, &statement("Deny", &action, "*"))
            .await;
        let answer = call_as(
            &emulator.router,
            &access_key_id,
            operation.route,
            &operation.body,
        )
        .await;
        assert_refused(
            &case,
            &answer,
            &user_arn,
            &action,
            &resource,
            Refusal::ExplicitDeny,
        );

        // 4. That document deleted: the grant the first document still carries is in force
        //    again, so the identical call is served.
        let case = format!("{} once the Deny is deleted", operation.name);
        emulator.delete_user_policy(&case, GUARD_POLICY).await;
        let answer = call_as(
            &emulator.router,
            &access_key_id,
            operation.route,
            &operation.body,
        )
        .await;
        assert_served(&case, &answer);

        // 5. And the GRANT deleted in turn: the principal is back to carrying nothing about
        //    this action, so the request falls to the default deny again.
        let case = format!("{} once the grant is deleted too", operation.name);
        emulator.delete_user_policy(&case, GRANT_POLICY).await;
        let answer = call_as(
            &emulator.router,
            &access_key_id,
            operation.route,
            &operation.body,
        )
        .await;
        assert_refused(
            &case,
            &answer,
            &user_arn,
            &action,
            &resource,
            Refusal::DefaultDeny,
        );
    }
}

/// THE RULE: an explicit `Deny` that is ABOUT the request overrides the grant however it is
/// written — by the action's own name, by a wildcard over this service's actions or over
/// every action, and scoped to the record the request addresses; a `Deny` about some OTHER
/// request (another catalog's records, another action of this service) leaves the grant
/// standing.
#[tokio::test]
async fn an_explicit_deny_overrides_the_grant_however_it_is_written() {
    let probe = Emulator::new();
    let (catalog, listing_id, product_id, offer_id) = catalog_records(&probe.router).await;

    for operation in under_test(&listing_id, &product_id, &offer_id) {
        let action = format!("{ACTION_PREFIX}:{}", operation.name);
        let resource = resource_of(&catalog, &operation.record);
        // Another action of this service, for the row whose `Deny` is about a different
        // request than the one made.
        let other_action = UNDER_TEST
            .iter()
            .map(|name| format!("{ACTION_PREFIX}:{name}"))
            .find(|candidate| *candidate != action)
            .expect("the suite names more than one operation");

        // (case, the Deny's Action, the Deny's Resource, whether it is about this request)
        let rows: Vec<(&str, String, String, bool)> = vec![
            (
                "a Deny of exactly this action on every resource",
                action.clone(),
                "*".to_string(),
                true,
            ),
            (
                "a Deny of every action of this service",
                format!("{ACTION_PREFIX}:*"),
                "*".to_string(),
                true,
            ),
            (
                "a Deny of every action of every service",
                "*".to_string(),
                "*".to_string(),
                true,
            ),
            (
                "a Deny scoped to the record the request addresses",
                action.clone(),
                resource.clone(),
                true,
            ),
            (
                "a Deny of this action on another catalog's records",
                action.clone(),
                resource_of("AnotherCatalog", &operation.record),
                false,
            ),
            (
                "a Deny of another action of this service",
                other_action,
                "*".to_string(),
                false,
            ),
        ];

        // One engine per operation. Every row rewrites BOTH documents under the same two
        // names, so a row is judged on its own grant and its own Deny and cannot pass on
        // another row's write.
        let emulator = Emulator::new();
        let fixture = format!("{} with a Deny", operation.name);
        let (user_arn, access_key_id) = emulator.given_user_with_no_policy(&fixture).await;

        for (row, deny_action, deny_resource, overrides) in rows {
            let case = format!("{} with {row}", operation.name);
            emulator
                .put_user_policy(&case, GRANT_POLICY, &statement("Allow", &action, "*"))
                .await;
            emulator
                .put_user_policy(
                    &case,
                    GUARD_POLICY,
                    &statement("Deny", &deny_action, &deny_resource),
                )
                .await;

            let answer = call_as(
                &emulator.router,
                &access_key_id,
                operation.route,
                &operation.body,
            )
            .await;
            if overrides {
                assert_refused(
                    &case,
                    &answer,
                    &user_arn,
                    &action,
                    &resource,
                    Refusal::ExplicitDeny,
                );
            } else {
                assert_served(&case, &answer);
            }
        }
    }
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a policy document's braces and quotes survive the round
/// trip into IAM.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The text of the first `<name>…</name>` element of an awsQuery response. Every element
/// this suite reads is a leaf carrying an id or an ARN, so there is no nesting to track.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

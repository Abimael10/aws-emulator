// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListPrincipalPolicies`, the DEPRECATED spelling of
//! `ListAttachedPolicies`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name, with the principal travelling in the
//! `x-amzn-iot-principal` header the model binds it to and `isAscendingOrder` / `pageSize`
//! / `marker` in the query string — so what is asserted is what an AWS SDK client would
//! see, not an internal function's return value.
//!
//! The rule under test: the listing REPORTS the one persisted policy-attachment relation
//! the attach operations write, for the principal itself (this spelling models no
//! `recursive` member, so a thing group's inherited policies are no part of it), ordered
//! by when each attachment was created in the direction `ascendingOrder` asks for, paged
//! by `pageSize` and resumed by a `marker` the same listing issued; it reports the policy
//! registry's own ARN for each row; it writes nothing; and it is only ever about a
//! principal that exists, so a well-formed principal naming a resource this account does
//! not hold is refused `ResourceNotFoundException` (404) and a principal of no supported
//! form `InvalidRequestException` (400) — never an empty list standing in for "no such
//! principal".
//!
//! The tables span the class the rule generalises over rather than one recorded request:
//! each principal form the modelled member documents (a certificate ARN, a thing group
//! ARN and a Cognito identity), both directions, every page size from one row per page to
//! more rows than exist plus the modelled maximum, policy names whose creation order
//! differs from their name order, and markers no listing issued.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";
/// An IoT policy document every policy in this suite is created with: the document is not
/// what this suite is about, so one valid document serves every case.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:*","Resource":"*"}]}"#;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` of an error body.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `policyName` of every entry of the answer, in the order the listing reported
    /// them.
    fn policy_names(&self) -> Vec<String> {
        self.entries()
            .iter()
            .map(|entry| member(entry, "policyName"))
            .collect()
    }

    /// The `policyArn` of every entry of the answer, in the order the listing reported
    /// them.
    fn policy_arns(&self) -> Vec<String> {
        self.entries()
            .iter()
            .map(|entry| member(entry, "policyArn"))
            .collect()
    }

    /// The `policies` list of the answer, or an empty list when the member is absent.
    fn entries(&self) -> Vec<Value> {
        self.body
            .get("policies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the answer carries a `policies` member at all, present and empty included.
    fn has_policies_member(&self) -> bool {
        self.body.get("policies").is_some()
    }

    /// The `nextMarker` of the answer, absent when the page is not truncated.
    fn next_marker(&self) -> Option<String> {
        self.body
            .get("nextMarker")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// One string member of a listing entry, or the empty string when it is absent.
fn member(entry: &Value, name: &str) -> String {
    entry
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// One request, with any number of extra headers — the legacy plane carries its
    /// principal in one of them.
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": POLICY_DOCUMENT })),
            &[],
        )
        .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a policy is attached to.
    async fn create_certificate(&self, set_as_active: bool) -> Reply {
        self.call(
            "POST",
            &format!("/keys-and-certificate?setAsActive={set_as_active}"),
            None,
            &[],
        )
        .await
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`.
    async fn create_thing_group(&self, name: &str, request: &Value) -> Reply {
        self.call("POST", &format!("/thing-groups/{name}"), Some(request), &[])
            .await
    }

    /// `AttachPrincipalPolicy`: `PUT /principal-policies/{policyName}` carrying the
    /// principal in the header the model binds it to.
    async fn attach_principal_policy(&self, policy_name: &str, principal: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/principal-policies/{policy_name}"),
            None,
            &[("x-amzn-iot-principal", principal)],
        )
        .await
    }

    /// `DetachPrincipalPolicy`: `DELETE /principal-policies/{policyName}`.
    async fn detach_principal_policy(&self, policy_name: &str, principal: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/principal-policies/{policy_name}"),
            None,
            &[("x-amzn-iot-principal", principal)],
        )
        .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target in the body
    /// — the CURRENT spelling of the same effect.
    async fn attach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
            &[],
        )
        .await
    }

    /// `ListAttachedPolicies`: `POST /attached-policies/{target}`, the current spelling of
    /// this listing, whose target is ONE path segment.
    async fn list_attached_policies(&self, target: &str, recursive: bool) -> Reply {
        let suffix = if recursive { "?recursive=true" } else { "" };
        self.call(
            "POST",
            &format!(
                "/attached-policies/{}{suffix}",
                crate::policy_attachments::as_path_segment(target)
            ),
            None,
            &[],
        )
        .await
    }

    /// `ListPrincipalPolicies`: `GET /principal-policies`, with the principal in the
    /// header and the direction, page size and marker in the query string. `ascending` is
    /// `None` for a request that names no direction at all, which the model defaults to
    /// `false`.
    async fn list_principal_policies(
        &self,
        principal: &str,
        ascending: Option<bool>,
        page_size: Option<i32>,
        marker: Option<&str>,
    ) -> Reply {
        let mut query: Vec<String> = Vec::new();
        if let Some(ascending) = ascending {
            query.push(format!("isAscendingOrder={ascending}"));
        }
        if let Some(page_size) = page_size {
            query.push(format!("pageSize={page_size}"));
        }
        if let Some(marker) = marker {
            query.push(format!("marker={marker}"));
        }
        let path = if query.is_empty() {
            "/principal-policies".to_string()
        } else {
            format!("/principal-policies?{}", query.join("&"))
        };
        self.call("GET", &path, None, &[("x-amzn-iot-principal", principal)])
            .await
    }

    /// Every policy of `principal`, walked page by page at `page_size` until the listing
    /// issues no further marker: the whole listing in the order it reported it.
    async fn walk_principal_policies(
        &self,
        principal: &str,
        ascending: Option<bool>,
        page_size: Option<i32>,
    ) -> Vec<String> {
        let mut walked: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        // One more page than rows can exist, so a listing that never stops fails the test
        // instead of looping.
        for _ in 0..64 {
            let page = self
                .list_principal_policies(principal, ascending, page_size, marker.as_deref())
                .await;
            assert_eq!(
                page.status, 200,
                "case ascending={ascending:?} pageSize={page_size:?}: a page must answer \
                 200, got {} {:?}",
                page.status, page.body
            );
            if let Some(size) = page_size {
                assert!(
                    page.policy_names().len() <= size as usize,
                    "case ascending={ascending:?} pageSize={page_size:?}: a page must hold \
                     at most the page size, got {:?}",
                    page.policy_names()
                );
            }
            walked.extend(page.policy_names());
            match page.next_marker() {
                None => return walked,
                Some(next) => marker = Some(next),
            }
        }
        panic!(
            "case ascending={ascending:?} pageSize={page_size:?}: the listing never stopped, \
             reported {walked:?}"
        );
    }
}

/// The ARN a policy created in this suite's region carries.
fn policy_arn(policy_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:policy/{policy_name}")
}

/// The ARN of a thing group created in this suite's region.
fn thing_group_arn(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{name}")
}

/// The ARN of a certificate in this suite's region, for a certificate id this account does
/// not hold.
fn certificate_arn(certificate_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{certificate_id}")
}

/// Create `policy_name`, asserting the create succeeded: every listing case starts from
/// policies that exist.
async fn given_policy(emulator: &Emulator, policy_name: &str) {
    let created = emulator.create_policy(policy_name).await;
    assert_eq!(
        created.status, 200,
        "creating the policy {policy_name} must succeed, got {} {:?}",
        created.status, created.body
    );
}

/// Issue a certificate, answering its `certificateArn`.
async fn given_certificate(emulator: &Emulator, set_as_active: bool) -> String {
    let issued = emulator.create_certificate(set_as_active).await;
    assert_eq!(
        issued.status, 200,
        "issuing a certificate must succeed, got {} {:?}",
        issued.status, issued.body
    );
    issued
        .body
        .get("certificateArn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Create a thing group, answering its `thingGroupArn`.
async fn given_thing_group(emulator: &Emulator, name: &str, request: &Value) -> String {
    let created = emulator.create_thing_group(name, request).await;
    assert_eq!(
        created.status, 200,
        "creating the thing group {name} must succeed, got {} {:?}",
        created.status, created.body
    );
    created
        .body
        .get("thingGroupArn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Attach each policy in turn, asserting each attach succeeded: the ORDER of this sequence
/// is the creation order the listing then reports.
async fn given_attachments(emulator: &Emulator, principal: &str, policy_names: &[&str]) {
    for policy_name in policy_names {
        given_policy(emulator, policy_name).await;
        let attached = emulator.attach_principal_policy(policy_name, principal).await;
        assert_eq!(
            attached.status, 200,
            "attaching {policy_name} to {principal} must answer 200, got {} {:?}",
            attached.status, attached.body
        );
    }
}

/// The listing reports the principal's attachments in the order they were CREATED, in the
/// direction the request asks for, with the policy registry's own ARN beside each name —
/// for every principal form the modelled member documents.
///
/// The policy names of each row are deliberately NOT in creation order alphabetically, so
/// a listing ordered by name rather than by creation fails.
#[tokio::test]
async fn the_listing_reports_a_principals_attachments_in_creation_order() {
    let emulator = Emulator::new();

    let active_certificate = given_certificate(&emulator, true).await;
    let inactive_certificate = given_certificate(&emulator, false).await;
    let group = given_thing_group(&emulator, "listed-group", &json!({})).await;

    // One row per principal form; the attachment sequence of each is its own, and none of
    // them is in name order.
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "an ACTIVE certificate ARN",
            active_certificate,
            vec!["zeta-policy", "alpha-policy", "mu-policy"],
        ),
        (
            "an INACTIVE certificate ARN",
            inactive_certificate,
            vec!["second_policy", "First.Policy"],
        ),
        (
            "a thing group ARN",
            group,
            vec!["group+policy=2", "group-policy-1"],
        ),
        (
            "a Cognito identity",
            format!("{REGION}:0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"),
            vec!["cognito.policy.z", "cognito.policy.a", "cognito.policy.m"],
        ),
    ];

    for (label, principal, attached_in_order) in &cases {
        given_attachments(&emulator, principal, attached_in_order).await;

        let ascending: Vec<String> = attached_in_order
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let descending: Vec<String> = ascending.iter().rev().cloned().collect();

        // `ascendingOrder` is modelled `default false`, so a request naming no direction
        // is the descending one.
        for (direction, asked, expected) in [
            ("ascending", Some(true), &ascending),
            ("descending", Some(false), &descending),
            ("defaulted", None, &descending),
        ] {
            let listed = emulator
                .list_principal_policies(principal, asked, None, None)
                .await;
            assert_eq!(
                listed.status, 200,
                "case {label} {direction}: the listing must answer 200, got {} {:?}",
                listed.status, listed.body
            );
            assert_eq!(
                &listed.policy_names(),
                expected,
                "case {label} {direction}: expected {expected:?}, got {:?}",
                listed.policy_names()
            );
            let expected_arns: Vec<String> =
                expected.iter().map(|name| policy_arn(name)).collect();
            assert_eq!(
                listed.policy_arns(),
                expected_arns,
                "case {label} {direction}: expected the policy registry's own ARNs \
                 {expected_arns:?}, got {:?}",
                listed.policy_arns()
            );
            assert_eq!(
                listed.next_marker(),
                None,
                "case {label} {direction}: an untruncated page carries no nextMarker, got \
                 {:?}",
                listed.next_marker()
            );
        }

        // A read changes nothing a later caller sees: the same question asked again has
        // the same answer, and the current spelling of the listing reports the same set.
        let again = emulator
            .list_principal_policies(principal, Some(true), None, None)
            .await;
        assert_eq!(
            again.policy_names(),
            ascending,
            "case {label}: a second read must report the same rows, got {:?}",
            again.policy_names()
        );
        let current = emulator.list_attached_policies(principal, false).await;
        assert_eq!(
            current.policy_names().into_iter().collect::<BTreeSet<_>>(),
            ascending.iter().cloned().collect::<BTreeSet<_>>(),
            "case {label}: both spellings read one relation, got {:?}",
            current.policy_names()
        );
    }
}

/// Every page size pages the SAME listing in the SAME order: the pages walked end to end
/// are the whole listing, each row once, in both directions; an untruncated page carries
/// no marker; and a marker this listing did not issue — including one issued by the other
/// direction — is refused 400 rather than resuming something else.
#[tokio::test]
async fn every_page_size_walks_the_whole_listing_in_order() {
    let emulator = Emulator::new();

    // A Cognito identity needs no resource of its own, so the rows cost one attach each.
    let principal = format!("{REGION}:bbbb1111-1111-1111-1111-111111111111");
    // Seven rows, attached in an order that is not their name order, and fewer than the
    // ceiling a target may hold.
    let attached_in_order = [
        "paged-g", "paged-a", "paged-f", "paged-b", "paged-e", "paged-c", "paged-d",
    ];
    given_attachments(&emulator, &principal, &attached_in_order).await;

    let ascending: Vec<String> = attached_in_order
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let descending: Vec<String> = ascending.iter().rev().cloned().collect();

    // Every page size from one row per page to more rows than exist, the modelled maximum
    // (`pageSize` is `range 1..250`) and no page size at all.
    for page_size in [None, Some(1), Some(2), Some(3), Some(7), Some(8), Some(250)] {
        for (direction, asked, expected) in [
            ("ascending", Some(true), &ascending),
            ("descending", Some(false), &descending),
            ("defaulted", None, &descending),
        ] {
            let walked = emulator
                .walk_principal_policies(&principal, asked, page_size)
                .await;
            assert_eq!(
                &walked, expected,
                "case {direction} pageSize={page_size:?}: the walk must report the whole \
                 listing in order, expected {expected:?}, got {walked:?}"
            );
        }
    }

    // A marker is only a position in the listing that issued it. The first page of the
    // ascending listing issues one; the descending listing refuses it, because the rows it
    // walks are not the rows that position was computed over.
    let first = emulator
        .list_principal_policies(&principal, Some(true), Some(2), None)
        .await;
    let ascending_marker = first
        .next_marker()
        .expect("a truncated page must carry a nextMarker");
    let crossed = emulator
        .list_principal_policies(&principal, Some(false), Some(2), Some(&ascending_marker))
        .await;
    assert_eq!(
        crossed.status, 400,
        "case a marker from the other direction: expected 400, got {} {:?}",
        crossed.status, crossed.body
    );

    // Markers no listing issued. Each satisfies the modelled `Marker` pattern
    // `^[A-Za-z0-9+/]+={0,2}$` — one outside it is refused by the framework's own
    // constraint check before any handler runs.
    for marker in ["QUJD", "0000", "7a7a7a", "ABCD=="] {
        let refused = emulator
            .list_principal_policies(&principal, Some(true), None, Some(marker))
            .await;
        assert_eq!(
            refused.status, 400,
            "case marker={marker:?}: expected 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case marker={marker:?}: expected InvalidRequestException, got {:?}",
            refused.error_code
        );
    }
}

/// The listing reports exactly what the relation holds, and nothing it does not: a
/// principal with no attachment answers an EMPTY list, a detached pair is gone, a pair the
/// current spelling recorded is reported, and a thing group's INHERITED policies are no
/// part of this listing — the deprecated spelling models no `recursive` member.
#[tokio::test]
async fn the_listing_reports_the_relation_and_only_the_relation() {
    let emulator = Emulator::new();

    let certificate = given_certificate(&emulator, true).await;

    // A principal that exists and holds nothing: "nothing is attached" is an answer.
    let empty = emulator
        .list_principal_policies(&certificate, Some(true), None, None)
        .await;
    assert_eq!(
        empty.status, 200,
        "a principal holding no policy must answer 200, got {} {:?}",
        empty.status, empty.body
    );
    assert!(
        empty.has_policies_member() && empty.policy_names().is_empty(),
        "a principal holding no policy must answer an empty policies list, got {:?}",
        empty.body
    );

    given_attachments(&emulator, &certificate, &["legacy-recorded"]).await;
    given_policy(&emulator, "current-recorded").await;
    let attached = emulator
        .attach_policy("current-recorded", &certificate)
        .await;
    assert_eq!(
        attached.status, 200,
        "AttachPolicy must answer 200, got {} {:?}",
        attached.status, attached.body
    );
    assert_eq!(
        emulator
            .list_principal_policies(&certificate, Some(true), None, None)
            .await
            .policy_names(),
        vec![
            "legacy-recorded".to_string(),
            "current-recorded".to_string()
        ],
        "both spellings write one relation, in the order they recorded it"
    );

    // What a detach removes, the listing stops reporting; what it leaves, it keeps
    // reporting in the position it was created in.
    let detached = emulator
        .detach_principal_policy("legacy-recorded", &certificate)
        .await;
    assert_eq!(
        detached.status, 200,
        "DetachPrincipalPolicy must answer 200, got {} {:?}",
        detached.status, detached.body
    );
    assert_eq!(
        emulator
            .list_principal_policies(&certificate, Some(true), None, None)
            .await
            .policy_names(),
        vec!["current-recorded".to_string()],
        "the detached pair must be gone and the other left"
    );

    // A group hierarchy: the parent's policy governs the child, which is what the CURRENT
    // spelling reports when asked recursively — and what this one never reports, because
    // it has no such member to ask with.
    let parent = given_thing_group(&emulator, "parent-group", &json!({})).await;
    let child = given_thing_group(
        &emulator,
        "child-group",
        &json!({ "parentGroupName": "parent-group" }),
    )
    .await;
    given_attachments(&emulator, &parent, &["parent-policy"]).await;
    given_attachments(&emulator, &child, &["child-policy"]).await;

    assert_eq!(
        emulator
            .list_principal_policies(&child, Some(true), None, None)
            .await
            .policy_names(),
        vec!["child-policy".to_string()],
        "the listing reports the principal's OWN attachments"
    );
    assert_eq!(
        emulator
            .list_principal_policies(&parent, Some(true), None, None)
            .await
            .policy_names(),
        vec!["parent-policy".to_string()],
        "an ancestor's listing reports its own attachments"
    );
    // The current spelling, asked recursively, does report the inherited one: the two
    // listings differ in what they walk, not in what the relation holds.
    assert_eq!(
        emulator
            .list_attached_policies(&child, true)
            .await
            .policy_names()
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["child-policy".to_string(), "parent-policy".to_string()]),
        "the recursive spelling reports the inherited policy"
    );
}

/// The listing is only ever about a principal that exists: a well-formed principal naming
/// a resource this account does not hold is refused 404, and a principal of no supported
/// form 400 — never an empty list standing in for "no such principal".
#[tokio::test]
async fn a_principal_this_account_does_not_hold_is_refused() {
    let emulator = Emulator::new();

    given_thing_group(&emulator, "known-group", &json!({})).await;

    // A certificate id this account holds no certificate under: 64 hex characters, the
    // modelled `CertificateId` form (`length 64..64, pattern ^(0x)?[a-fA-F0-9]+$`).
    let not_found: Vec<(&str, String, String)> = vec![
        (
            "an unknown certificate principal",
            certificate_arn(&"ab".repeat(32)),
            "The certificate given in the principal does not exist.".to_string(),
        ),
        (
            "an unknown thing group principal",
            thing_group_arn("no-such-group"),
            format!("ThingGroup {ACCOUNT_ID}:no-such-group not found."),
        ),
    ];

    for (label, principal, message) in &not_found {
        let refused = emulator
            .list_principal_policies(principal, Some(true), None, None)
            .await;
        assert_eq!(
            refused.status, 404,
            "case {label}: the listing must refuse it 404, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {label}: expected ResourceNotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            &refused.message(),
            message,
            "case {label}: expected the message {message:?}, got {:?}",
            refused.message()
        );
    }

    // Strings that name no principal of this account's policies at all. The third field is
    // whether the value is an ARN, which is what decides whether the refusal names it: the
    // API names the address it read, and names nothing when it read none.
    let invalid = vec![
        ("a bare name", "plant-floor".to_string(), false),
        ("a path with no ARN prefix", "cert/abc".to_string(), false),
        (
            "an ARN of another service",
            format!("arn:aws:iam::{ACCOUNT_ID}:role/device"),
            true,
        ),
        (
            "an IoT ARN of a resource no policy attaches to",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/sensor-1"),
            true,
        ),
        (
            "a certificate ARN with no id",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/"),
            true,
        ),
        (
            "an IoT ARN with no resource type",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert"),
            true,
        ),
        ("a Cognito identity with no id", format!("{REGION}:"), false),
        (
            "a Cognito identity with no region",
            ":abcdef".to_string(),
            false,
        ),
    ];

    for (label, principal, names_the_principal) in &invalid {
        let refused = emulator
            .list_principal_policies(principal, Some(true), None, None)
            .await;
        assert_eq!(
            refused.status, 400,
            "case {label}: the listing must refuse it 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case {label}: expected InvalidRequestException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            // One wording rule, stated once: the principal-policy plane resolves its
            // `principal` through the same resolver the target plane resolves `target`
            // with.
            crate::policy_attachments::invalid_target_message(principal, *names_the_principal),
            "case {label}: unexpected refusal message {:?}",
            refused.message()
        );
    }
}

/// An attachment is CREATED once: re-attaching a pair the relation already holds keeps the
/// position the first attach recorded, through either spelling of the attach, and leaves
/// every other row where it was — so the listing's order stays the order the attachments
/// were created in, in both directions.
#[tokio::test]
async fn re_attaching_a_recorded_pair_keeps_the_position_it_was_created_at() {
    let emulator = Emulator::new();

    let principal = format!("{REGION}:cccc2222-2222-2222-2222-222222222222");
    // The order the three pairs were created in, which is the order the listing must keep
    // whatever is re-attached afterwards.
    let created_in_order = ["first-policy", "second-policy", "third-policy"];
    given_attachments(&emulator, &principal, &created_in_order).await;
    // One more policy that exists but is not attached yet: a genuinely NEW attachment made
    // after the re-attaches must still take the position after the three above, which is
    // what shows the re-attach spent no position of its own.
    given_policy(&emulator, "fourth-policy").await;

    let ascending: Vec<String> = created_in_order
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    // Each row re-attaches one of the recorded pairs, through one of the two spellings of
    // the attach — both write the one relation, so both must leave the positions alone.
    let cases: Vec<(&str, &str, bool)> = vec![
        ("the first pair, legacy spelling", "first-policy", true),
        ("the first pair, current spelling", "first-policy", false),
        ("a middle pair, legacy spelling", "second-policy", true),
        ("the last pair, current spelling", "third-policy", false),
    ];

    for (label, policy_name, through_legacy) in &cases {
        let attached = if *through_legacy {
            emulator
                .attach_principal_policy(policy_name, &principal)
                .await
        } else {
            emulator.attach_policy(policy_name, &principal).await
        };
        assert_eq!(
            attached.status, 200,
            "case {label}: the re-attach must answer 200, got {} {:?}",
            attached.status, attached.body
        );

        for (direction, asked, expected) in [
            ("ascending", Some(true), ascending.clone()),
            (
                "descending",
                Some(false),
                ascending.iter().rev().cloned().collect(),
            ),
        ] {
            let listed = emulator
                .list_principal_policies(&principal, asked, None, None)
                .await;
            assert_eq!(
                listed.policy_names(),
                expected,
                "case {label} {direction}: a re-attach must move nothing, expected \
                 {expected:?}, got {:?}",
                listed.policy_names()
            );
        }
    }

    // A pair that really is new takes the next position, after every pair recorded before
    // it: no re-attach spent one, and no two attachments share one.
    let attached = emulator
        .attach_principal_policy("fourth-policy", &principal)
        .await;
    assert_eq!(
        attached.status, 200,
        "attaching a new policy must answer 200, got {} {:?}",
        attached.status, attached.body
    );
    let mut expected = ascending.clone();
    expected.push("fourth-policy".to_string());
    let listed = emulator
        .list_principal_policies(&principal, Some(true), None, None)
        .await;
    assert_eq!(
        listed.policy_names(),
        expected,
        "the newly attached pair must come last, expected {expected:?}, got {:?}",
        listed.policy_names()
    );
}

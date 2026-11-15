// SPDX-License-Identifier: CC0-1.0

//! `UpdateEmailIdentityPolicy`, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization
//! are all exercised, and reads back through `GetEmailIdentityPolicies` (and
//! `GetEmailIdentity`, which reports the same map) what the write persisted.
//!
//! The rule under test: an accepted update stores the submitted document under the
//! submitted policy name of the named identity, REPLACING the document that name held
//! and changing nothing else about the identity's policy map, so a later read reports
//! the document the most recent accepted write carried; an identity the account does
//! not hold, a policy name the identity carries no policy under, and a name or a
//! document that breaks a documented policy rule are each refused with the modelled
//! error for that class and leave the stored map exactly as it was.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    email_identity_not_found_unquoted, identity_key, identity_policy_not_found,
    ComAmazonawsSesv2SimpleemailserviceV2State, MAX_IDENTITY_POLICIES, MAX_POLICY_BYTES,
    VERIFICATION_SETTLE,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// One answered request: the status, the error shape when there was one, and the
/// parsed body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
    ///
    /// restJson1 resolves the error shape from `x-amzn-errortype`; the JSON body's
    /// `__type` carries the same name, so either is read.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        // A shape name may arrive namespaced (`com.amazonaws.sesv2#NotFound`) or with
        // a trailing URL, as the protocol allows.
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message the error carried, for an assertion that wants to show it.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `Policies` map the response reports, as name against document.
    fn policies(&self) -> BTreeMap<String, String> {
        self.body
            .get("Policies")
            .and_then(Value::as_object)
            .map(|members| {
                members
                    .iter()
                    .map(|(name, document)| {
                        (
                            name.clone(),
                            document.as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The emulated service behind its router, plus the shared engine so a test can read
/// what a handler persisted.
struct Ses {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://email.us-east-1.amazonaws.com{path}"))
            .header("host", "email.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            // The SigV4 credential scope is what routes the request to the `ses`
            // signing name; the signature itself is not checked here.
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/us-east-1/ses/aws4_request, SignedHeaders=host, Signature=00",
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
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
            error_type,
            body,
        }
    }

    /// Start verifying an identity, so the account holds it.
    async fn create_identity(&self, identity: &str) -> Reply {
        self.call(
            "POST",
            "/v2/email/identities",
            Some(json!({"EmailIdentity": identity})),
        )
        .await
    }

    /// `CreateEmailIdentityPolicy`: the name is a path label, the document the body.
    async fn create_policy(&self, identity: &str, name: &str, document: &str) -> Reply {
        self.call(
            "POST",
            &format!("/v2/email/identities/{identity}/policies/{name}"),
            Some(json!({"Policy": document})),
        )
        .await
    }

    /// The operation under test: `PUT` on the policy's own path.
    async fn update_policy(&self, identity: &str, name: &str, document: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/v2/email/identities/{identity}/policies/{name}"),
            Some(json!({"Policy": document})),
        )
        .await
    }

    async fn get_policies(&self, identity: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v2/email/identities/{identity}/policies"),
            None,
        )
        .await
    }

    /// `GetEmailIdentity`, which reports the same map among the identity's other
    /// attributes: the two readers answer from one record, so they must agree.
    async fn get_identity(&self, identity: &str) -> Reply {
        self.call("GET", &format!("/v2/email/identities/{identity}"), None)
            .await
    }

    /// The policies the store itself holds for `identity`, for a case whose
    /// assertion must not depend on a reporting operation.
    fn stored_policies(&self, identity: &str) -> BTreeMap<String, String> {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let store = state.lock();
        store
            .identities
            .get(&identity_key(identity))
            .map(|record| record.policies.clone())
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    /// Hold the clock-owed verification transition off, so a case about an identity
    /// that is still `PENDING` cannot race the span.
    fn hold_verification_pending(&self, identity: &str) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let record = store
            .identities
            .get_mut(&identity_key(identity))
            .unwrap_or_else(|| panic!("case {identity:?}: the identity must exist in the store"));
        record.verification_due_at =
            SystemTime::now() + VERIFICATION_SETTLE + Duration::from_secs(3600);
    }
}

/// A sending-authorization policy whose statement names `principal`: an `Effect`, the
/// SES action it authorizes and the principal it authorizes to use the identity.
fn policy_for(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Action\":\"ses:SendEmail\",\"Principal\":{{\"AWS\":\"{principal}\"}}}}]}}"
    )
}

/// A policy document of exactly `target` bytes, padded inside its `Sid`, for the rows
/// that drive the published size ceiling.
fn policy_of_size(target: usize) -> String {
    let prefix = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"";
    let suffix = "\",\"Effect\":\"Allow\",\"Action\":\"ses:SendEmail\",\
                  \"Principal\":{\"AWS\":\"arn:aws:iam::000000000000:root\"}}]}";
    let padding = target.saturating_sub(prefix.len() + suffix.len());
    let document = format!("{prefix}{}{suffix}", "a".repeat(padding));
    assert_eq!(
        document.len(),
        target,
        "case {target:?}: the padded document must be exactly that many bytes, got {}",
        document.len()
    );
    document
}

/// One row of the replacement table: the identity the policy was created on, the
/// spelling the update addresses it with, the policy name, the document the create
/// stored, the document the update submits, and the document a read must report
/// afterwards.
struct ReplaceCase {
    case: &'static str,
    created: &'static str,
    spelled: &'static str,
    name: String,
    before: String,
    update: String,
    expected: String,
}

#[tokio::test]
async fn an_accepted_update_replaces_the_document_stored_under_the_policy_name() {
    // `PolicyName` is modelled `length 1..64`, so this is a name of exactly 64
    // characters: 60 letters, the underscore, and three more admitted characters.
    let long_name = format!("{}_{}", "a".repeat(60), "Z-9");
    // The rows differ along what the rule generalises over: the kind of identity the
    // policy hangs off, how the request spells it, the form of the policy name, and
    // how the submitted document differs from the stored one — a broadened statement
    // (the class the recorded trace carried: a new `Sid` and a single action string
    // replaced by a list of two), a narrowed one, a different principal, a different
    // policy language version, a document at the published size ceiling, and a
    // document whose insignificant whitespace is dropped on the way in.
    let rows: [ReplaceCase; 8] = [
        ReplaceCase {
            case: "a broadened statement: a new Sid and a list of two actions",
            created: "example.com",
            spelled: "example.com",
            name: "AllowSend".to_string(),
            before: "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"AllowSend\",\
                     \"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"arn:aws:iam::000000000000:root\"},\
                     \"Action\":\"ses:SendEmail\",\
                     \"Resource\":\"arn:aws:ses:us-east-1:000000000000:identity/example.com\"}]}"
                .to_string(),
            update: "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"AllowSendAndRawSend\",\
                     \"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"arn:aws:iam::000000000000:root\"},\
                     \"Action\":[\"ses:SendEmail\",\"ses:SendRawEmail\"],\
                     \"Resource\":\"arn:aws:ses:us-east-1:000000000000:identity/example.com\"}]}"
                .to_string(),
            expected: "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"AllowSendAndRawSend\",\
                       \"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"arn:aws:iam::000000000000:root\"},\
                       \"Action\":[\"ses:SendEmail\",\"ses:SendRawEmail\"],\
                       \"Resource\":\"arn:aws:ses:us-east-1:000000000000:identity/example.com\"}]}"
                .to_string(),
        },
        ReplaceCase {
            case: "a narrowed statement: two statements replaced by one",
            created: "narrow.example",
            spelled: "narrow.example",
            name: "Narrow".to_string(),
            before: "{\"Version\":\"2012-10-17\",\"Statement\":[\
                     {\"Effect\":\"Allow\",\"Action\":\"ses:SendEmail\",\"Principal\":\"*\"},\
                     {\"Effect\":\"Allow\",\"Action\":\"ses:SendRawEmail\",\"Principal\":\"*\"}]}"
                .to_string(),
            update: policy_for("arn:aws:iam::000000000000:root"),
            expected: policy_for("arn:aws:iam::000000000000:root"),
        },
        ReplaceCase {
            case: "an address identity, with a different principal",
            created: "sender@example.com",
            spelled: "sender@example.com",
            name: "AllowPartner".to_string(),
            before: policy_for("arn:aws:iam::000000000000:root"),
            update: policy_for("arn:aws:iam::000000000000:user/partner"),
            expected: policy_for("arn:aws:iam::000000000000:user/partner"),
        },
        ReplaceCase {
            case: "a domain spelled in another case, which names the same identity",
            created: "Example.ORG",
            spelled: "example.org",
            name: "Policy-1".to_string(),
            before: policy_for("*"),
            update: policy_for("arn:aws:iam::000000000000:root"),
            expected: policy_for("arn:aws:iam::000000000000:root"),
        },
        ReplaceCase {
            case: "a policy name at the modelled 64-character ceiling",
            created: "long@example.net",
            spelled: "long@example.net",
            name: long_name.clone(),
            before: policy_for("*"),
            update: policy_for("arn:aws:iam::000000000000:root"),
            expected: policy_for("arn:aws:iam::000000000000:root"),
        },
        ReplaceCase {
            case: "a different policy language version and an inline Statement object",
            created: "version.example",
            spelled: "version.example",
            name: "policy_2_v3".to_string(),
            before: policy_for("*"),
            update: "{\"Version\":\"2008-10-17\",\"Statement\":{\"Effect\":\"Allow\",\
                     \"Action\":\"ses:SendRawEmail\",\"Principal\":\"*\"}}"
                .to_string(),
            expected: "{\"Version\":\"2008-10-17\",\"Statement\":{\"Effect\":\"Allow\",\
                       \"Action\":\"ses:SendRawEmail\",\"Principal\":\"*\"}}"
                .to_string(),
        },
        ReplaceCase {
            case: "a document at exactly the published 4 KB ceiling",
            created: "big.example",
            spelled: "big.example",
            name: "Big".to_string(),
            before: policy_for("*"),
            update: policy_of_size(MAX_POLICY_BYTES),
            expected: policy_of_size(MAX_POLICY_BYTES),
        },
        ReplaceCase {
            case: "a document carrying whitespace between its tokens",
            created: "pretty.example",
            spelled: "pretty.example",
            name: "Pretty".to_string(),
            before: policy_for("*"),
            update: "{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {\n      \
                     \"Sid\": \"Allow the partner account\",\n      \"Effect\": \"Allow\",\n      \
                     \"Action\": \"ses:SendEmail\",\n      \
                     \"Principal\": {\"AWS\": \"*\"}\n    }\n  ]\n}"
                .to_string(),
            // Only the whitespace OUTSIDE a string literal is dropped: the spaces
            // inside the `Sid` value are part of that value and survive.
            expected: "{\"Version\":\"2012-10-17\",\"Statement\":[{\
                       \"Sid\":\"Allow the partner account\",\"Effect\":\"Allow\",\
                       \"Action\":\"ses:SendEmail\",\"Principal\":{\"AWS\":\"*\"}}]}"
                .to_string(),
        },
    ];

    for row in rows {
        let ReplaceCase {
            case,
            created,
            spelled,
            name,
            before,
            update,
            expected,
        } = row;
        let ses = Ses::new();
        let start = ses.create_identity(created).await;
        assert_eq!(
            start.status, 200,
            "case {case:?}: the identity must be created first, got {} {:?}",
            start.status, start.body
        );
        let stored = ses.create_policy(created, &name, &before).await;
        assert_eq!(
            stored.status, 200,
            "case {case:?}: the policy must be created first, got {} {:?}",
            stored.status, stored.body
        );

        let reply = ses.update_policy(spelled, &name, &update).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            reply.status, reply.body
        );
        assert!(
            reply
                .body
                .as_object()
                .is_none_or(serde_json::Map::is_empty),
            "case {case:?}: the response models no members, got {:?}",
            reply.body
        );

        let read = ses.get_policies(created).await;
        let policies = read.policies();
        assert_eq!(
            policies.get(&name).map(String::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the updated document back under {name:?}, got {policies:?}"
        );
        assert_eq!(
            policies.len(),
            1,
            "case {case:?}: the update replaced the one entry rather than adding another, got {policies:?}"
        );
        assert_eq!(
            ses.get_identity(created).await.policies(),
            policies,
            "case {case:?}: both readers answer from one record, got {policies:?}"
        );
    }
}

/// One row of the missing-policy table: the policies the identity already carries, as
/// name against the principal its document authorizes, and the policy name the update
/// addresses — a name the identity carries none under.
struct MissingPolicyCase {
    case: &'static str,
    carried: &'static [(&'static str, &'static str)],
    name: &'static str,
}

#[tokio::test]
async fn an_update_of_a_policy_name_the_identity_does_not_carry_is_refused() {
    // An update addresses one policy OF an identity, so a name the identity carries
    // none under names no policy: the rows differ along what the identity does carry —
    // nothing at all, another policy that must be left alone, a name that differs from
    // a carried one only in case (policy names are compared exactly), and a name
    // carried by ANOTHER identity of the same account.
    let rows: [MissingPolicyCase; 4] = [
        MissingPolicyCase {
            case: "an identity carrying no policy at all",
            carried: &[],
            name: "FirstPolicy",
        },
        MissingPolicyCase {
            case: "an identity carrying another policy",
            carried: &[("Existing", "existing")],
            name: "SecondPolicy",
        },
        MissingPolicyCase {
            case: "a name differing from a carried one only in case",
            carried: &[("Policy", "carried")],
            name: "policy",
        },
        MissingPolicyCase {
            case: "a name carried by another identity of the account",
            carried: &[("Other", "other")],
            name: "CarriedElsewhere",
        },
    ];

    for MissingPolicyCase {
        case,
        carried,
        name,
    } in rows
    {
        let ses = Ses::new();
        ses.create_identity("example.com").await;
        for (carried_name, principal) in carried {
            let reply = ses
                .create_policy(
                    "example.com",
                    carried_name,
                    &policy_for(&format!("arn:aws:iam::000000000000:user/{principal}")),
                )
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: the carried policy must be created first, got {} {:?}",
                reply.status, reply.body
            );
        }
        // The last row's name is carried by a DIFFERENT identity, which is a different
        // policy: the update still finds none on the identity it addresses.
        ses.create_identity("elsewhere.example").await;
        ses.create_policy("elsewhere.example", "CarriedElsewhere", &policy_for("*"))
            .await;

        let document = policy_for("arn:aws:iam::000000000000:root");
        let reply = ses.update_policy("example.com", name, &document).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: expected 404 NotFoundException, got {} {:?}",
            reply.status,
            reply.body
        );
        let expected = identity_policy_not_found(name);
        assert_eq!(
            reply.message(),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            reply.message()
        );

        let policies = ses.get_policies("example.com").await.policies();
        assert!(
            !policies.contains_key(name),
            "case {case:?}: the refused update stored nothing under {name:?}, got {policies:?}"
        );
        assert_eq!(
            policies.len(),
            carried.len(),
            "case {case:?}: the identity's map is as it was, got {policies:?}"
        );
        for (carried_name, principal) in carried {
            assert_eq!(
                policies.get(*carried_name).map(String::as_str),
                Some(policy_for(&format!("arn:aws:iam::000000000000:user/{principal}")).as_str()),
                "case {case:?}: the policy {carried_name:?} the identity carried is untouched, got {policies:?}"
            );
        }
        let elsewhere = ses.get_policies("elsewhere.example").await.policies();
        assert_eq!(
            elsewhere.get("CarriedElsewhere").map(String::as_str),
            Some(policy_for("*").as_str()),
            "case {case:?}: the other identity's policy of that name is untouched, got {elsewhere:?}"
        );
    }
}

#[tokio::test]
async fn the_most_recent_accepted_update_is_what_a_read_reports() {
    let ses = Ses::new();
    ses.create_identity("example.com").await;
    ses.create_policy("example.com", "Policy", &policy_for("*"))
        .await;

    // A sequence of writes over one name, including the same document twice: every
    // read reports the document the latest accepted write carried, so a repeated
    // write changes nothing and a later one is never shadowed by an earlier one.
    for (case, document) in [
        ("the first update", policy_for("arn:aws:iam::000000000000:root")),
        (
            "a second, different document",
            policy_for("arn:aws:iam::000000000000:user/partner"),
        ),
        (
            "the same document again",
            policy_for("arn:aws:iam::000000000000:user/partner"),
        ),
        ("back to the wildcard principal", policy_for("*")),
    ] {
        let reply = ses.update_policy("example.com", "Policy", &document).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            reply.status, reply.body
        );
        let policies = ses.get_policies("example.com").await.policies();
        assert_eq!(
            policies.get("Policy").map(String::as_str),
            Some(document.as_str()),
            "case {case:?}: expected the document the latest write carried, got {policies:?}"
        );
        assert_eq!(
            policies.len(),
            1,
            "case {case:?}: the identity still carries one policy, got {policies:?}"
        );
    }
}

#[tokio::test]
async fn an_identity_still_pending_verification_takes_an_update() {
    let ses = Ses::new();
    ses.create_identity("pending.example").await;
    ses.create_policy("pending.example", "WhilePending", &policy_for("*"))
        .await;
    // The transition the clock owes is held off, so this case cannot race the span:
    // what the rule turns on is that the account HOLDS the identity, which it does
    // from the moment verification started.
    ses.hold_verification_pending("pending.example");

    let document = policy_for("arn:aws:iam::000000000000:root");
    let reply = ses
        .update_policy("pending.example", "WhilePending", &document)
        .await;
    assert_eq!(
        reply.status, 200,
        "case \"an identity still pending verification\": expected 200, got {} {:?}",
        reply.status, reply.body
    );
    let read = ses.get_identity("pending.example").await;
    assert_eq!(
        read.body.get("VerificationStatus").and_then(Value::as_str),
        Some("PENDING"),
        "case \"an identity still pending verification\": the identity is still PENDING, got {:?}",
        read.body
    );
    assert_eq!(
        read.policies().get("WhilePending"),
        Some(&document),
        "case \"an identity still pending verification\": expected the updated document back, got {:?}",
        read.policies()
    );
}

#[tokio::test]
async fn an_update_for_an_identity_the_account_does_not_hold_is_refused() {
    let ses = Ses::new();
    ses.create_identity("example.com").await;
    ses.create_policy("example.com", "Policy", &policy_for("*"))
        .await;

    // The rule is about the identity the policy hangs off, so it holds for every kind
    // of name the account holds no identity for — including a subdomain of a domain it
    // does hold, and an address at that domain, each of which is a different identity.
    for case in [
        "absent.example",
        "nobody@example.net",
        "mail.example.com",
        "sender@example.com",
    ] {
        let reply = ses
            .update_policy(case, "Policy", &policy_for("*"))
            .await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: expected 404 NotFoundException, got {} {:?}",
            reply.status,
            reply.body
        );
        let expected = email_identity_not_found_unquoted(case);
        assert_eq!(
            reply.message(),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            reply.message()
        );
        assert!(
            ses.stored_policies(case).is_empty(),
            "case {case:?}: nothing is persisted for an identity the account does not hold, got {:?}",
            ses.stored_policies(case)
        );
    }

    // The identity that DOES exist was not touched by any of those calls.
    let policies = ses.get_policies("example.com").await.policies();
    assert_eq!(
        policies.get("Policy").map(String::as_str),
        Some(policy_for("*").as_str()),
        "case \"the held identity\": its policy still stands, got {policies:?}"
    );
}

#[tokio::test]
async fn an_update_breaking_a_documented_policy_rule_leaves_the_stored_document() {
    // The document and name rules are the policy's, so an update is refused on each of
    // them exactly as a create is: the rows differ along the rule broken — the name's
    // character set, the IAM resource-policy grammar, the published size ceiling, and
    // the ARN a resource element has to name. A refused update must neither replace the
    // document that stands nor insert one under a new name.
    let standing = policy_for("*");
    let rows: [(&str, &str, String, Option<String>); 6] = [
        (
            "a policy name outside the documented character set",
            "policy.one",
            policy_for("arn:aws:iam::000000000000:root"),
            None,
        ),
        (
            "a policy name carrying a wildcard",
            "Policy*",
            policy_for("arn:aws:iam::000000000000:root"),
            None,
        ),
        (
            "a document that is not JSON at all",
            "Policy",
            "let me send".to_string(),
            Some("Unable to parse policy.".to_string()),
        ),
        (
            "a document that is JSON but carries no statement",
            "Policy",
            "{}".to_string(),
            Some("Unable to parse policy.".to_string()),
        ),
        (
            "a document one byte past the published ceiling",
            "Policy",
            policy_of_size(MAX_POLICY_BYTES + 1),
            None,
        ),
        (
            "a document whose resource element is not an ARN",
            "Policy",
            "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
             \"Action\":\"ses:SendEmail\",\"Principal\":{\"AWS\":\"*\"},\
             \"Resource\":\"example.com\"}]}"
                .to_string(),
            Some("Invalid ARN: ARNs must start with 'arn:': example.com".to_string()),
        ),
    ];

    for (case, name, document, expected_message) in rows {
        let ses = Ses::new();
        ses.create_identity("example.com").await;
        ses.create_policy("example.com", "Policy", &standing).await;

        let reply = ses.update_policy("example.com", name, &document).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {} {:?}",
            reply.status,
            reply.body
        );
        if let Some(expected) = expected_message {
            assert_eq!(
                reply.message(),
                expected,
                "case {case:?}: expected the message {expected:?}, got {:?}",
                reply.message()
            );
        }

        let stored = ses.stored_policies("example.com");
        assert_eq!(
            stored.get("Policy").map(String::as_str),
            Some(standing.as_str()),
            "case {case:?}: the document that stood is unchanged, got {stored:?}"
        );
        assert_eq!(
            stored.len(),
            1,
            "case {case:?}: a refused update inserts nothing, got {stored:?}"
        );
    }
}

#[tokio::test]
async fn an_update_never_grows_the_identity_s_bounded_policy_map() {
    let ses = Ses::new();
    ses.create_identity("example.com").await;
    for index in 0..MAX_IDENTITY_POLICIES {
        let name = format!("policy_{index}");
        let reply = ses
            .create_policy("example.com", &name, &policy_for("*"))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {name:?}: expected 200 below the ceiling, got {} {:?}",
            reply.status, reply.body
        );
    }

    // An update only ever replaces, so a name the identity already carries is updated
    // with the set at its documented ceiling, and the set does not grow.
    let document = policy_for("arn:aws:iam::000000000000:root");
    let reply = ses
        .update_policy("example.com", "policy_0", &document)
        .await;
    assert_eq!(
        reply.status, 200,
        "case \"a carried name at the ceiling\": expected 200, got {} {:?}",
        reply.status, reply.body
    );
    let policies = ses.get_policies("example.com").await.policies();
    assert_eq!(
        policies.get("policy_0").map(String::as_str),
        Some(document.as_str()),
        "case \"a carried name at the ceiling\": expected the updated document, got {policies:?}"
    );
    assert_eq!(
        policies.len(),
        MAX_IDENTITY_POLICIES,
        "case \"a carried name at the ceiling\": the replacement added nothing, got {policies:?}"
    );

    // A name the identity carries none under names no policy, at the ceiling as
    // anywhere else, so no update can add an entry to the map.
    let reply = ses
        .update_policy("example.com", "one_too_many", &policy_for("*"))
        .await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (404, "NotFoundException"),
        "case \"a name the identity does not carry at the ceiling\": expected 404 NotFoundException, got {} {:?}",
        reply.status,
        reply.body
    );
    let stored = ses.stored_policies("example.com");
    assert_eq!(
        stored.len(),
        MAX_IDENTITY_POLICIES,
        "case \"a name the identity does not carry at the ceiling\": the refused update added nothing, got {stored:?}"
    );
}

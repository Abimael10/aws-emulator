// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the `ProfileDetail` every profile-returning operation
//! answers with: what a create DEFAULTS, what it records as the creating principal, and
//! what it leaves OFF the wire.
//!
//! Three rules are under test, each applied to a table of create bodies rather than to one
//! recorded request:
//!
//! 1. A response states what the profile HOLDS: a member the request did not set and the
//!    service does not default carries no value on the record and is omitted from the body,
//!    never rendered as an empty list or a cleared flag. The members the service DOES
//!    default are `durationSeconds` ("if this value is not provided, the default value will
//!    be 3600", `smithy_query(query="shape", shape="ProfileDetail")` member
//!    `durationSeconds`), the two enablement-style flags, and the attribute mappings.
//! 2. A new profile already maps its certificate fields: the service seeds one entry per
//!    value of the modelled `CertificateField` enum ([`DEFAULT_MAPPINGS`]), which is state
//!    the profile holds — every read reports it and the mapping writes compose on top of it.
//! 3. `createdBy` names the principal the request's CREDENTIAL speaks for, as an IAM
//!    principal ARN — the identity `sts:GetCallerIdentity` answers for that same
//!    credential — so it is never a constant and never the bare account id.
//!
//! Every expectation below is derived from the request the row sent (or asked of the token
//! service), so the suite states the rules rather than one recorded body. The rows differ
//! along what the rules generalise over: which optional members a create carries, whether a
//! list member is set to the empty list or left unset, whether a flag is set false or left
//! unset, which operation answers the detail, and which kind of credential signed the
//! create.
//!
//! The harness below is local to this suite, as in each sibling suite.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;

/// The Region this suite signs for.
const REGION: &str = "us-west-2";

/// The date stamp the requests here are signed with: a credential scope is what carries
/// the Region, so every call needs one.
const SIGNED_DATE: &str = "20260101";

/// The account every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The access key id this suite signs with unless a row varies it.
const ACCESS_KEY_ID: &str = "AKIDTEST";

/// The `durationSeconds` a create that omits it leaves on the profile: "In requests, if
/// this value is not provided, the default value will be 3600"
/// (`smithy_query(query="shape", shape="ProfileDetail")`, member `durationSeconds`).
const DEFAULT_DURATION_SECONDS: i64 = 3600;

/// The members a create may set that the service does NOT default: a request that omits one
/// leaves the profile holding no value for it, so the response carries no such member.
///
/// `requireInstanceProperties` is "unused, saved for future use"
/// (`smithy_query(query="shape", shape="ProfileDetail")`), `sessionPolicy` is "a session
/// policy that applies to the trust boundary of the vended session credentials" and
/// `managedPolicyArns` "a list of managed policy ARNs that apply to the vended session
/// credentials" — none of the three has a documented or recorded default.
const UNDEFAULTED_MEMBERS: [&str; 3] = [
    "managedPolicyArns",
    "requireInstanceProperties",
    "sessionPolicy",
];

/// The attribute mappings a profile is created with, as `(certificateField, [specifier])`
/// pairs in the order a response lists them.
///
/// One entry per value of the modelled `CertificateField` enum
/// (`smithy_query(query="shape", shape="CertificateField")`:
/// `["x509Subject", "x509Issuer", "x509SAN"]`). The SPECIFIERS are recorded behavior: the
/// model documents `attributeMappings` only as "a mapping applied to the authenticating
/// end-entity certificate" and states no defaults, so this catalog is what the service is
/// recorded seeding a new profile with.
const DEFAULT_MAPPINGS: [(&str, &[&str]); 3] = [
    ("x509Issuer", &["*"]),
    ("x509SAN", &["DNS", "URI", "Name/*"]),
    ("x509Subject", &["*"]),
];

/// The default mappings as a response carries them.
fn default_mappings() -> Value {
    Value::Array(
        DEFAULT_MAPPINGS
            .iter()
            .map(|(field, specifiers)| {
                json!({
                    "certificateField": field,
                    "mappingRules": specifiers
                        .iter()
                        .map(|specifier| json!({"specifier": specifier}))
                        .collect::<Vec<Value>>(),
                })
            })
            .collect(),
    )
}

/// The specifiers a field maps in one detail, or `None` when the detail holds no entry for
/// that field.
fn specifiers_in(case: &str, detail: &Value, field: &str) -> Option<Vec<String>> {
    detail
        .get("attributeMappings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected attributeMappings in {detail:?}"))
        .iter()
        .find(|entry| entry.get("certificateField").and_then(Value::as_str) == Some(field))
        .map(|entry| {
            entry
                .get("mappingRules")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
                .iter()
                .map(|rule| {
                    rule.get("specifier")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
}

/// One HTTP answer: the status and the parsed JSON body.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The `profile` member of a successful answer.
    fn profile(&self, case: &str) -> Value {
        self.body
            .pointer("/profile")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a profile member, got {self:?}"))
    }
}

/// A client over one emulator: Roles Anywhere and the token service on ONE shared engine,
/// so the identity STS answers for a credential is resolved from the very records this
/// service resolves it from.
struct Client {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Client {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_rolesanywhere_rolesanywhere::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// One control-plane call, signed (in the routing sense) with `access_key_id`.
    async fn send_as(
        &self,
        access_key_id: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> Answer {
        let payload = body
            .map(|value| bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/{SIGNED_DATE}/{REGION}/\
                     rolesanywhere/aws4_request, SignedHeaders=host, Signature=deadbeef"
                ),
            )
            .body(payload)
            .expect("build request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Answer { status, body }
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        self.send_as(ACCESS_KEY_ID, method, uri, body).await
    }

    async fn create_profile(&self, body: Value) -> Answer {
        self.send("POST", "/profiles", Some(body)).await
    }

    async fn get_profile(&self, id: &str) -> Answer {
        self.send("GET", &format!("/profile/{id}"), None).await
    }

    async fn update_profile(&self, id: &str, body: Value) -> Answer {
        self.send("PATCH", &format!("/profile/{id}"), Some(body))
            .await
    }

    async fn disable_profile(&self, id: &str) -> Answer {
        self.send("POST", &format!("/profile/{id}/disable"), None)
            .await
    }

    async fn enable_profile(&self, id: &str) -> Answer {
        self.send("POST", &format!("/profile/{id}/enable"), None)
            .await
    }

    async fn delete_profile(&self, id: &str) -> Answer {
        self.send("DELETE", &format!("/profile/{id}"), None).await
    }

    async fn put_mapping(&self, id: &str, field: &str, specifiers: &[&str]) -> Answer {
        self.send(
            "PUT",
            &format!("/profiles/{id}/mappings"),
            Some(json!({
                "certificateField": field,
                "mappingRules": specifiers
                    .iter()
                    .map(|specifier| json!({"specifier": specifier}))
                    .collect::<Vec<Value>>(),
            })),
        )
        .await
    }

    async fn delete_mapping(&self, id: &str, field: &str, specifiers: &[&str]) -> Answer {
        let mut query = format!("?certificateField={field}");
        for specifier in specifiers {
            query.push_str(&format!("&specifiers={specifier}"));
        }
        self.send(
            "DELETE",
            &format!("/profiles/{id}/mappings{query}"),
            None,
        )
        .await
    }

    /// The element of the `ListProfiles` projection that reports this profile.
    async fn listed(&self, case: &str, id: &str) -> Value {
        let answer = self.send("GET", "/profiles", None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected ListProfiles 200, got {answer:?}"
        );
        answer
            .body
            .get("profiles")
            .and_then(Value::as_array)
            .and_then(|elements| {
                elements
                    .iter()
                    .find(|entry| entry.get("profileId").and_then(Value::as_str) == Some(id))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case:?}: {id} must appear in {answer:?}"))
    }

    /// The ARN `sts:GetCallerIdentity` answers for a request signed with `access_key_id`:
    /// the identity that credential resolves to, asked of the service that owns the
    /// question.
    async fn identity_arn(&self, case: &str, access_key_id: &str) -> String {
        let (status, body) = self
            .sts_call(access_key_id, "GetCallerIdentity", &[])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: sts:GetCallerIdentity must answer the signing credential, got \
             {status} with {body}"
        );
        element(case, &body, "Arn")
    }

    /// One STS awsQuery call, signed (in the routing sense) with `access_key_id`.
    async fn sts_call(
        &self,
        access_key_id: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2011-06-15");
        for (name, value) in params {
            body.push_str(&format!("&{name}={value}"));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://sts.amazonaws.com/")
            .header("host", "sts.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/{SIGNED_DATE}/{REGION}/sts/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(&response.into_body()).to_string(),
        )
    }

    /// Record that the account holds `access_key_id` for the IAM user `user_name` — the
    /// user record `iam:CreateUser` writes and the access-key record
    /// `iam:CreateAccessKey` writes — and answer that user's ARN.
    ///
    /// The user is given an inline policy allowing this service's actions, as
    /// `iam:PutUserPolicy` writes one: on AWS an IAM user whose policies grant nothing may
    /// perform no action of any service, so a fixture user that is to MAKE calls has to
    /// carry the grant for them. What the grant is read for is
    /// `tests/all/identity_authorization.rs`; here it only puts the caller in a position to
    /// exercise the rules this suite is about.
    fn hold_long_term_key_for(&self, user_name: &str, access_key_id: &str) -> String {
        let arn = format!("arn:aws:iam::{ACCOUNT}:user/{user_name}");
        let state =
            iam::service_state(&*self.engine).expect("the in-memory engine carries the IAM store");
        let mut account = state.lock();
        account.users.insert(
            iam::name_key(user_name),
            User {
                path: "/".to_string(),
                user_name: user_name.to_string(),
                user_id: sts::state::unique_id("AIDA", &arn),
                arn: arn.clone(),
                create_date: ::aws_smithy_types::DateTime::from_secs(0),
                password_last_used: None,
                permissions_boundary: None,
                tags: None,
            },
        );
        account.user_inline_policies.insert(
            iam::name_key(user_name),
            std::collections::BTreeMap::from([(
                iam::name_key("rolesanywhere-caller"),
                iam::InlinePolicy {
                    policy_name: "rolesanywhere-caller".to_string(),
                    policy_document: json!({
                        "Version": "2012-10-17",
                        "Statement": [
                            {"Effect": "Allow", "Action": "rolesanywhere:*", "Resource": "*"}
                        ]
                    })
                    .to_string(),
                },
            )]),
        );
        account.access_keys.insert(
            access_key_id.to_string(),
            iam::AccessKeyRecord {
                user_key: iam::name_key(user_name),
                status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
                create_date: ::aws_smithy_types::DateTime::from_secs(0),
            },
        );
        arn
    }
}

/// The text of the one `<name>…</name>` element an awsQuery response carries.
fn element(case: &str, body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body
        .find(&open)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
        + open.len();
    let end = body[start..]
        .find(&close)
        .unwrap_or_else(|| panic!("case {case:?}: no </{name}> in {body}"))
        + start;
    body[start..end].to_string()
}

/// An IAM role ARN a profile can vend credentials for.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Assert a call answered the status its operation answers with.
fn ok(case: &str, expected_status: u16, answer: &Answer) {
    assert_eq!(
        answer.status, expected_status,
        "case {case:?}: expected HTTP {expected_status}, got {answer:?}"
    );
}

/// The id a create minted.
fn minted_id(case: &str, detail: &Value) -> String {
    detail
        .get("profileId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no profileId in {detail:?}"))
        .to_string()
}

/// Apply rules 1 to 3 to one `ProfileDetail`, against the create body it came from.
fn assert_detail_follows_the_create(case: &str, body: &Value, detail: &Value) {
    assert_eq!(
        detail.get("attributeMappings"),
        Some(&default_mappings()),
        "case {case:?}: a created profile maps every certificate field by default, got \
         {detail:?}"
    );

    // The table signs with a key no store holds a record for, which is the account's own
    // principal — asked of the service that owns the identity question rather than spelled
    // here. `created_by_is_the_principal_the_requests_credential_resolves_to` below walks
    // the other kinds of credential.
    let creator = sts::state::ambient_identity().arn;
    assert_eq!(
        detail.get("createdBy").and_then(Value::as_str),
        Some(creator.as_str()),
        "case {case:?}: createdBy names the creating principal by ARN, got {detail:?}"
    );

    // The members the service defaults: present with the request's value, or with the
    // default when the request said nothing.
    assert_eq!(
        detail.get("enabled"),
        Some(&Value::Bool(
            body.get("enabled").and_then(Value::as_bool).unwrap_or(false)
        )),
        "case {case:?}: a profile is enabled only when the create asked for it, got {detail:?}"
    );
    assert_eq!(
        detail.get("acceptRoleSessionName"),
        Some(&Value::Bool(
            body.get("acceptRoleSessionName")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )),
        "case {case:?}: expected the acceptRoleSessionName the create left, got {detail:?}"
    );
    assert_eq!(
        detail.get("durationSeconds").and_then(Value::as_i64),
        Some(
            body.get("durationSeconds")
                .and_then(Value::as_i64)
                .unwrap_or(DEFAULT_DURATION_SECONDS)
        ),
        "case {case:?}: expected the durationSeconds the create left, got {detail:?}"
    );

    // The members it does not default: exactly what the request carried, and nothing at
    // all when the request carried nothing.
    for member in UNDEFAULTED_MEMBERS {
        assert_eq!(
            detail.get(member),
            body.get(member),
            "case {case:?}: {member} must be what the create set, and absent when the create \
             set nothing, got {detail:?}"
        );
    }
}

/// The create bodies the first two tables walk: the rows span which optional members the
/// create carries, a list member set to the empty list against one left unset, a flag set
/// false against one left unset, and both published `durationSeconds` bounds.
fn create_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "nothing-optional",
            json!({"name": "minimal", "roleArns": [role_arn("Minimal")]}),
        ),
        (
            "enabled-asked-for",
            json!({"name": "enabled", "roleArns": [role_arn("Enabled")], "enabled": true}),
        ),
        (
            "enabled-refused",
            json!({"name": "disabled", "roleArns": [role_arn("Disabled")], "enabled": false}),
        ),
        (
            "managed-policies-set",
            json!({
                "name": "with policies",
                "roleArns": [role_arn("Policies")],
                "managedPolicyArns": [
                    "arn:aws:iam::aws:policy/ReadOnlyAccess",
                    format!("arn:aws:iam::{ACCOUNT}:policy/Custom"),
                ],
            }),
        ),
        // The empty list is a value the caller CHOSE: `managedPolicyArns` is
        // `@length(0, 50)`, so it is a legal list and has to survive as one, which is the
        // case that separates "set to empty" from "not set".
        (
            "managed-policies-set-empty",
            json!({
                "name": "empty policies",
                "roleArns": [role_arn("EmptyPolicies")],
                "managedPolicyArns": [],
            }),
        ),
        (
            "instance-properties-required",
            json!({
                "name": "instance properties",
                "roleArns": [role_arn("Properties")],
                "requireInstanceProperties": true,
            }),
        ),
        // A flag set false is likewise a value the caller chose, not an absence.
        (
            "instance-properties-set-false",
            json!({
                "name": "properties false",
                "roleArns": [role_arn("PropertiesFalse")],
                "requireInstanceProperties": false,
            }),
        ),
        (
            "session-policy-set",
            json!({
                "name": "session policy",
                "roleArns": [role_arn("Session")],
                "sessionPolicy": "{\"Version\":\"2012-10-17\",\"Statement\":[]}",
            }),
        ),
        // `durationSeconds` is `@range(900, 43200)`
        // (`smithy_query(query="operation", operation="CreateProfile")`): both bounds.
        (
            "duration-at-the-floor",
            json!({
                "name": "floor duration",
                "roleArns": [role_arn("Floor")],
                "durationSeconds": 900,
            }),
        ),
        (
            "duration-at-the-ceiling",
            json!({
                "name": "ceiling duration",
                "roleArns": [role_arn("Ceiling")],
                "durationSeconds": 43200,
            }),
        ),
        (
            "every-optional-member",
            json!({
                "name": "every member",
                "roleArns": [role_arn("Full"), role_arn("team/Nested")],
                "managedPolicyArns": ["arn:aws:iam::aws:policy/AdministratorAccess"],
                "sessionPolicy": "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Deny\"}]}",
                "durationSeconds": 7200,
                "enabled": true,
                "requireInstanceProperties": true,
                "acceptRoleSessionName": true,
                "tags": [{"key": "env", "value": "test"}],
            }),
        ),
        (
            "no-role-arns",
            json!({"name": "no roles", "roleArns": []}),
        ),
    ]
}

/// A create defaults what the service defaults, records the creating principal by ARN, and
/// omits every member nothing set — and the read paths report the same record.
#[tokio::test]
async fn a_created_profile_carries_the_service_defaults_and_omits_what_nothing_set() {
    for (case, body) in create_rows() {
        let client = Client::new();
        let created = client.create_profile(body.clone()).await;
        ok(case, 201, &created);
        let detail = created.profile(case);
        assert_detail_follows_the_create(case, &body, &detail);

        // The create's answer is the stored record, not an echo: the read and the list
        // projection report it member for member.
        let id = minted_id(case, &detail);
        let read = client.get_profile(&id).await;
        ok(&format!("{case}/get"), 200, &read);
        assert_eq!(
            read.profile(case),
            detail,
            "case {case:?}: GetProfile must report the record the create stored, got {read:?}"
        );
        assert_eq!(
            client.listed(case, &id).await,
            detail,
            "case {case:?}: the ListProfiles projection must report the same record"
        );
    }
}

/// Every operation that answers a `ProfileDetail` answers the SAME record: the write
/// echoes, the reads and the deletion receipt all carry the service defaults, the
/// principal-ARN `createdBy` and no member nothing set — and the receipt reports the
/// profile's state as the delete found it.
#[tokio::test]
async fn every_profile_returning_operation_answers_the_stored_record() {
    for (case, body) in create_rows() {
        let client = Client::new();
        let created = client.create_profile(body.clone()).await;
        ok(case, 201, &created);
        let id = minted_id(case, &created.profile(case));

        // An update writes the members it carries; everything rule 1 to 3 covers has to
        // survive it, so the echo is checked against the create body with the written
        // members folded in.
        let written = json!({
            "sessionPolicy": "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\"}]}",
            "durationSeconds": 7200,
        });
        let mut after_update = body.clone();
        for (member, value) in written.as_object().expect("the body is an object") {
            after_update[member.clone()] = value.clone();
        }

        let updated = client.update_profile(&id, written).await;
        ok(&format!("{case}/update"), 200, &updated);
        assert_detail_follows_the_create(
            &format!("{case}/update"),
            &after_update,
            &updated.profile(case),
        );

        let disabled = client.disable_profile(&id).await;
        ok(&format!("{case}/disable"), 200, &disabled);
        let mut after_disable = after_update.clone();
        after_disable["enabled"] = Value::Bool(false);
        assert_detail_follows_the_create(
            &format!("{case}/disable"),
            &after_disable,
            &disabled.profile(case),
        );

        let enabled = client.enable_profile(&id).await;
        ok(&format!("{case}/enable"), 200, &enabled);
        let mut after_enable = after_update.clone();
        after_enable["enabled"] = Value::Bool(true);
        let live = enabled.profile(case);
        assert_detail_follows_the_create(&format!("{case}/enable"), &after_enable, &live);

        // The last read before the delete, which the receipt has to agree with.
        let read = client.get_profile(&id).await;
        ok(&format!("{case}/read"), 200, &read);
        assert_eq!(
            read.profile(case),
            live,
            "case {case:?}: GetProfile must report what the enable wrote, got {read:?}"
        );

        let removed = client.delete_profile(&id).await;
        ok(&format!("{case}/delete"), 200, &removed);
        assert_eq!(
            removed.profile(case),
            live,
            "case {case:?}: the deletion receipt reports the profile as the delete found it, \
             got {removed:?}"
        );
        assert_detail_follows_the_create(
            &format!("{case}/delete"),
            &after_enable,
            &removed.profile(case),
        );
    }
}

/// Which credential a row's create is signed with.
#[derive(Debug, Clone, Copy)]
enum Credential {
    /// A long-term key no store holds a record for: the position the credential this
    /// emulator publishes at start is in.
    UnheldKey,
    /// A long-term key the account holds for one of its IAM users.
    IamUserKey { user: &'static str },
    /// The temporary credentials of an `sts:GetSessionToken` session, minted here for an
    /// IAM user's own long-term key.
    SessionToken { user: &'static str },
}

/// `createdBy` names the principal the request's credential resolves to — the identity the
/// token service answers for the same credential — and never a constant.
///
/// The rows span the kind of credential a create is signed with, and each IAM user's name
/// is deliberately unrelated to the text of the access key id it holds, so an identity
/// spelled out of that text cannot pass a row. A third principal only READS, so the ARN a
/// read reports must be the creator's and never the reader's.
#[tokio::test]
async fn created_by_is_the_principal_the_requests_credential_resolves_to() {
    let rows: [(&str, Credential); 4] = [
        ("key-no-store-holds", Credential::UnheldKey),
        (
            "key-held-for-a-user",
            Credential::IamUserKey { user: "Mary_Major" },
        ),
        (
            "key-held-for-another-user",
            Credential::IamUserKey { user: "Li_Juan" },
        ),
        (
            "session-of-a-users-key",
            Credential::SessionToken {
                user: "Saanvi_Sarkar",
            },
        ),
    ];

    let mut seen: Vec<String> = Vec::new();
    for (case, credential) in rows {
        let client = Client::new();
        // A reader that holds a key of its own, so the read path has a caller that is not
        // the creator.
        let reader_key = "AKIDPROFILEREADER";
        client.hold_long_term_key_for("Pat_Candella", reader_key);

        let access_key_id = match credential {
            Credential::UnheldKey => "AKIDNOSTOREHOLDSTHIS".to_string(),
            Credential::IamUserKey { user } => {
                let key = format!("AKIDHELDKEY{}", user.len());
                client.hold_long_term_key_for(user, &key);
                key
            }
            Credential::SessionToken { user } => {
                let long_term = format!("AKIDTOKENHOLDER{}", user.len());
                client.hold_long_term_key_for(user, &long_term);
                let (status, body) = client
                    .sts_call(&long_term, "GetSessionToken", &[])
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: sts:GetSessionToken must mint a session, got {status} with \
                     {body}"
                );
                element(case, &body, "AccessKeyId")
            }
        };

        let expected = client.identity_arn(case, &access_key_id).await;
        let created = client
            .send_as(
                &access_key_id,
                "POST",
                "/profiles",
                Some(json!({"name": "authored", "roleArns": [role_arn("Authored")]})),
            )
            .await;
        ok(case, 201, &created);
        let detail = created.profile(case);
        assert_eq!(
            detail.get("createdBy").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected createdBy {expected:?}, got {detail:?}"
        );

        // A read by another principal reports the CREATOR, because the value is stored.
        let id = minted_id(case, &detail);
        let read = client
            .send_as(reader_key, "GET", &format!("/profile/{id}"), None)
            .await;
        ok(&format!("{case}/read"), 200, &read);
        assert_eq!(
            read.profile(case).get("createdBy").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: a read reports the creating principal, got {read:?}"
        );
        seen.push(expected);
    }

    // Four credentials, four principals: the value is resolved per caller rather than
    // being one constant.
    let distinct: std::collections::BTreeSet<&String> = seen.iter().collect();
    assert_eq!(
        distinct.len(),
        seen.len(),
        "case \"one principal per credential\": expected a distinct createdBy per credential, \
         got {seen:?}"
    );
}

/// The default mappings are STATE the profile holds, not a value a read invents: a put adds
/// to the specifiers a field already maps, a delete can unmap a default specifier, and a
/// delete naming no specifier drops the field's default entry — each read back through
/// `GetProfile`.
///
/// The rows span which certificate field is written, whether the write adds to or removes
/// from the default specifiers, and whether a delete names specifiers or none at all.
#[tokio::test]
async fn the_default_mappings_are_state_the_mapping_writes_compose_on_top_of() {
    let client = Client::new();
    let created = client
        .create_profile(json!({"name": "mapped", "roleArns": [role_arn("Mapped")]}))
        .await;
    ok("create", 201, &created);
    let id = minted_id("create", &created.profile("create"));

    // (case, the field written, the specifiers a put carries, the specifiers a delete
    // names — `None` for a put row, an empty slice for a delete that names none)
    type Step<'a> = (&'a str, &'a str, Option<&'a [&'a str]>, Option<&'a [&'a str]>);
    let steps: Vec<Step> = vec![
        (
            "put-joins-the-default-subject-specifier",
            "x509Subject",
            Some(&["CN"]),
            None,
        ),
        (
            "put-repeating-a-default-specifier-adds-nothing",
            "x509Subject",
            Some(&["*"]),
            None,
        ),
        (
            "delete-unmaps-one-default-san-specifier",
            "x509SAN",
            None,
            Some(&["URI"]),
        ),
        (
            "delete-naming-none-drops-the-default-issuer-entry",
            "x509Issuer",
            None,
            Some(&[]),
        ),
    ];

    for (case, field, put, delete) in steps {
        let before = client.get_profile(&id).await;
        ok(&format!("{case}/before"), 200, &before);
        let mapped_before = specifiers_in(case, &before.profile(case), field).unwrap_or_default();

        let answer = match (put, delete) {
            (Some(specifiers), _) => client.put_mapping(&id, field, specifiers).await,
            (_, Some(specifiers)) => client.delete_mapping(&id, field, specifiers).await,
            _ => unreachable!("every row either puts or deletes"),
        };
        ok(case, 200, &answer);

        let expected: Option<Vec<String>> = match (put, delete) {
            (Some(specifiers), _) => {
                let mut after = mapped_before.clone();
                for specifier in specifiers {
                    if !after.iter().any(|held| held == specifier) {
                        after.push((*specifier).to_string());
                    }
                }
                Some(after)
            }
            (_, Some([])) => None,
            (_, Some(specifiers)) => {
                let after: Vec<String> = mapped_before
                    .iter()
                    .filter(|held| !specifiers.iter().any(|named| named == *held))
                    .cloned()
                    .collect();
                (!after.is_empty()).then_some(after)
            }
            _ => unreachable!("every row either puts or deletes"),
        };

        assert_eq!(
            specifiers_in(case, &answer.profile(case), field),
            expected,
            "case {case:?}: expected {field:?} to map {expected:?} after the write, got {:?}",
            answer.profile(case)
        );

        let read = client.get_profile(&id).await;
        ok(&format!("{case}/read"), 200, &read);
        assert_eq!(
            read.profile(case),
            answer.profile(case),
            "case {case:?}: the write must be stored, so the read agrees with it, got {read:?}"
        );
    }
}

/// One write a sequence applies to a stored profile.
#[derive(Debug, Clone)]
enum ProfileWrite {
    /// `UpdateProfile` carrying these members.
    Update(Value),
    /// `DisableProfile`.
    Disable,
    /// `EnableProfile`.
    Enable,
}

/// The members an echo reports beyond what [`assert_detail_follows_the_create`] covers: the
/// identifier the request addressed, and the two members a create and an update both write.
fn assert_identity_members(case: &str, expected: &Value, detail: &Value, id: &str) {
    assert_eq!(
        detail.get("profileId").and_then(Value::as_str),
        Some(id),
        "case {case:?}: the echo must report the addressed profile, got {detail:?}"
    );
    assert_eq!(
        detail.get("name"),
        expected.get("name"),
        "case {case:?}: expected the name the last write left, got {detail:?}"
    );
    assert_eq!(
        detail.get("roleArns"),
        expected.get("roleArns"),
        "case {case:?}: expected the roleArns the last write left, got {detail:?}"
    );
}

/// A session policy document a row's update submits: `sessionPolicy` is `@length(1, 100000)`
/// (`smithy_query(query="operation", operation="UpdateProfile")`), so any document in that
/// range is a legal value to store and report back.
fn session_policy(effect: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": "*", "Resource": "*"}],
    })
    .to_string()
}

/// Every write-path `ProfileDetail` echo answers the profile's whole state as that write
/// left it — the same body the read path answers immediately afterwards, service defaults,
/// principal-ARN `createdBy` and omitted unset members included — and the deletion receipt
/// reports the state the delete found, whatever sequence of writes came before it.
///
/// The rows span the ORDER of the writes, how many there are, whether one is repeated, which
/// members an update carries (both published `durationSeconds` bounds among them), and the
/// shape of the profile the sequence runs against, since each row takes a different create
/// body from [`create_rows`].
#[tokio::test]
async fn every_profile_write_echo_reports_the_state_the_write_left() {
    let bodies = create_rows();
    let sequences: Vec<(&str, Vec<ProfileWrite>)> = vec![
        (
            "update-then-disable",
            vec![
                ProfileWrite::Update(json!({
                    "durationSeconds": 7200,
                    "sessionPolicy": session_policy("Allow"),
                })),
                ProfileWrite::Disable,
            ],
        ),
        (
            "disable-then-update-then-enable",
            vec![
                ProfileWrite::Disable,
                ProfileWrite::Update(json!({
                    "managedPolicyArns": ["arn:aws:iam::aws:policy/ReadOnlyAccess"],
                })),
                ProfileWrite::Enable,
            ],
        ),
        (
            "enable-disable-enable",
            vec![
                ProfileWrite::Enable,
                ProfileWrite::Disable,
                ProfileWrite::Enable,
            ],
        ),
        (
            "disable-repeated-then-update",
            vec![
                ProfileWrite::Disable,
                ProfileWrite::Disable,
                ProfileWrite::Update(json!({
                    "name": "renamed by the update",
                    "acceptRoleSessionName": true,
                })),
            ],
        ),
        (
            "update-at-each-duration-bound-then-toggle",
            vec![
                ProfileWrite::Update(json!({"durationSeconds": 900})),
                ProfileWrite::Update(json!({"durationSeconds": 43200})),
                ProfileWrite::Disable,
                ProfileWrite::Enable,
            ],
        ),
        (
            "enable-then-update-every-writable-member",
            vec![
                ProfileWrite::Enable,
                ProfileWrite::Update(json!({
                    "name": "fully updated",
                    "roleArns": [role_arn("Updated"), role_arn("team/AlsoUpdated")],
                    "managedPolicyArns": [],
                    "sessionPolicy": session_policy("Deny"),
                    "durationSeconds": 1800,
                    "acceptRoleSessionName": false,
                })),
                ProfileWrite::Disable,
                ProfileWrite::Enable,
            ],
        ),
        (
            "one-update-and-nothing-else",
            vec![ProfileWrite::Update(json!({
                "sessionPolicy": session_policy("Allow"),
            }))],
        ),
    ];

    for (index, (case, writes)) in sequences.into_iter().enumerate() {
        let (shape, body) = &bodies[index % bodies.len()];
        let labelled_case = format!("{case}/over-{shape}");
        let client = Client::new();
        let created = client.create_profile(body.clone()).await;
        ok(&labelled_case, 201, &created);
        let id = minted_id(&labelled_case, &created.profile(&labelled_case));

        // What the profile holds, as the requests sent so far left it: the create body with
        // every later write folded in. Every expectation below is read off this, so the
        // table states the rule rather than one recorded body.
        let mut expected = body.clone();
        let mut last_state = created.profile(&labelled_case);

        for (step, write) in writes.iter().enumerate() {
            let labelled = format!("{labelled_case}/step-{step}");
            let answer = match write {
                ProfileWrite::Update(members) => {
                    for (member, value) in members
                        .as_object()
                        .expect("an update body is a JSON object")
                    {
                        expected[member.clone()] = value.clone();
                    }
                    client.update_profile(&id, members.clone()).await
                }
                ProfileWrite::Disable => {
                    expected["enabled"] = Value::Bool(false);
                    client.disable_profile(&id).await
                }
                ProfileWrite::Enable => {
                    expected["enabled"] = Value::Bool(true);
                    client.enable_profile(&id).await
                }
            };
            ok(&labelled, 200, &answer);
            let echo = answer.profile(&labelled);
            assert_detail_follows_the_create(&labelled, &expected, &echo);
            assert_identity_members(&labelled, &expected, &echo, &id);

            // The echo IS the stored record, so the read path answers the same body — a
            // client that takes the echo as the profile's current state reads what a
            // re-read would have told it.
            let read = client.get_profile(&id).await;
            ok(&labelled, 200, &read);
            assert_eq!(
                read.profile(&labelled),
                echo,
                "case {labelled:?}: GetProfile must report what the write echoed, got {read:?}"
            );
            assert_eq!(
                client.listed(&labelled, &id).await,
                echo,
                "case {labelled:?}: the ListProfiles projection must report the same record"
            );
            last_state = echo;
        }

        // The deletion receipt reports the profile as the delete found it — the state the
        // last write left, flag included — and the profile is then gone.
        let removed = client.delete_profile(&id).await;
        let delete_case = format!("{labelled_case}/delete");
        ok(&delete_case, 200, &removed);
        assert_eq!(
            removed.profile(&delete_case),
            last_state,
            "case {delete_case:?}: the receipt must report the state the delete found, got \
             {removed:?}"
        );
        let gone = client.get_profile(&id).await;
        assert_eq!(
            gone.status, 404,
            "case {delete_case:?}: the deleted profile must resolve to nothing, got {gone:?}"
        );
    }
}

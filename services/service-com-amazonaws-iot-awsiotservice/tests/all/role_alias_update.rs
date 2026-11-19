// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `UpdateRoleAlias`: what a mutation of a role alias writes
//! to the store every other role-alias operation reads, and how one addressed to a name
//! the account holds no alias under is refused.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rule under test: a single-resource mutation of this control plane is applied to
//! the record the requested name identifies and to nothing else — it replaces exactly
//! the optional members the request carried, leaves the resource's identity and every
//! member the request did not name as the create wrote them, and is read back by the
//! family's reader — while a name the account holds no such record under is refused the
//! operation's modelled not-found error, with the same sentence the family's reader
//! refuses a miss with, rather than a hollow success that writes nothing. Here that
//! means `UpdateRoleAlias` rewrites the `roleArn` and the `credentialDurationSeconds`
//! the request names, `DescribeRoleAlias` reports the rewritten record, and an update of
//! an alias that was never created or that `DeleteRoleAlias` removed answers 404
//! `ResourceNotFoundException`.
//!
//! The table spans what that rule generalises over rather than one recorded request:
//! which subset of the two optional members the update carries (each alone, both, and
//! neither), where in the modelled `range 900..43200` the duration sits (each published
//! bound and a value between them), the form and length of the alias name, whether the
//! create carried tags, and the region the calls are signed for.
//!
//! Replacing "exactly the members the request carried" is a FOLD over however many
//! updates a caller makes, so the tables below also span the SEQUENCE: after any run of
//! updates the record holds, for each member, the latest value an update named for it and
//! the created value for every member none named — which makes repeating one update a
//! no-op on everything but the record's last-modified stamp — and a member no
//! `UpdateRoleAlias` request can name at all, such as the alias's tag set, survives every
//! update untouched.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// The one query-value percent-encoder this test crate has: `ListTagsForResource` binds
// its `resourceArn` to the query string, so an ARN's separators travel encoded exactly as
// an SDK client sends them.
use crate::describe_endpoint::percent_encode;

/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The bounds of `CredentialDurationSeconds`, which is modelled `range 900..43200`
/// (`smithy_query(query="operation",operation="UpdateRoleAlias")`).
const MIN_CREDENTIAL_DURATION_SECONDS: i64 = 900;
const MAX_CREDENTIAL_DURATION_SECONDS: i64 = 43_200;

/// The `credentialDurationSeconds` an alias created without one carries: the member is
/// documented "The default value is 3,600 seconds"
/// (`smithy_query(query="doc",shape="CreateRoleAliasRequest",member="credentialDurationSeconds")`).
const DEFAULT_CREDENTIAL_DURATION_SECONDS: i64 = 3_600;

/// The longest legal `roleAlias`: the member is modelled `length 1..128`
/// (`smithy_query(query="operation",operation="UpdateRoleAlias")`).
const MAX_ROLE_ALIAS_LEN: usize = 128;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name
/// and `region`: the scope is how the router picks the claiming service, and how the
/// service picks the region its ARNs are scoped to. The signature itself is never
/// checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
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
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `roleAliasDescription` a `DescribeRoleAlias` success carries.
    fn description(&self) -> Option<&Value> {
        self.body.get("roleAliasDescription")
    }

    /// One member of that description, or `None` when the read failed.
    fn described(&self, name: &str) -> Option<&Value> {
        self.description().and_then(|value| value.get(name))
    }

    /// A numeric member of that description.
    fn described_number(&self, name: &str) -> Option<f64> {
        self.described(name).and_then(Value::as_f64)
    }

    /// The reported tag set, keyed by `Tag$Key`, or `None` when the response carries no
    /// `tags` member at all.
    ///
    /// The same walk as `authorizer_registry::resource_tagging`'s reader of a tag set,
    /// which is where the first copy of it lives: the two cannot be one `pub(crate)`
    /// helper, because a shared test module of this crate would have to be declared in
    /// `tests/all.rs`, which this tree does not accept an edit to.
    fn tags(&self) -> Option<BTreeMap<String, Option<String>>> {
        Some(
            self.body
                .get("tags")?
                .as_array()?
                .iter()
                .map(|tag| {
                    (
                        tag.get("Key")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tag.get("Value").and_then(Value::as_str).map(str::to_string),
                    )
                })
                .collect(),
        )
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>, region: &str) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
            .header("content-type", "application/json")
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

    /// `CreateRoleAlias`: `POST /role-aliases/{roleAlias}`.
    async fn create_in(&self, name: &str, body: &Value, region: &str) -> Reply {
        self.call("POST", &format!("/role-aliases/{name}"), Some(body), region)
            .await
    }

    async fn create(&self, name: &str, body: &Value) -> Reply {
        self.create_in(name, body, REGION).await
    }

    /// `UpdateRoleAlias`: `PUT /role-aliases/{roleAlias}`.
    async fn update_in(&self, name: &str, body: &Value, region: &str) -> Reply {
        self.call("PUT", &format!("/role-aliases/{name}"), Some(body), region)
            .await
    }

    async fn update(&self, name: &str, body: &Value) -> Reply {
        self.update_in(name, body, REGION).await
    }

    /// `DescribeRoleAlias`: `GET /role-aliases/{roleAlias}`.
    async fn describe(&self, name: &str) -> Reply {
        self.call("GET", &format!("/role-aliases/{name}"), None, REGION)
            .await
    }

    /// `DeleteRoleAlias`: `DELETE /role-aliases/{roleAlias}`.
    async fn delete(&self, name: &str) -> Reply {
        self.call("DELETE", &format!("/role-aliases/{name}"), None, REGION)
            .await
    }

    /// `ListTagsForResource`: `GET /tags?resourceArn={resourceArn}` — the reader of the
    /// tag set a role alias carries, which no `UpdateRoleAlias` member names.
    async fn list_tags(&self, resource_arn: &str, region: &str) -> Reply {
        self.call(
            "GET",
            &format!("/tags?resourceArn={}", percent_encode(resource_arn)),
            None,
            region,
        )
        .await
    }
}

/// The ARN of a role alias, as `CreateRoleAlias` mints it in the region the call was
/// signed for: `arn:aws:iot:<region>:<account>:rolealias/<roleAlias>`.
fn role_alias_arn(region: &str, name: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:rolealias/{name}")
}

/// The sentence every operation addressing ONE role alias refuses a name the account
/// holds no alias under with, as the service words it with the name taken from the
/// request.
fn missing_message(name: &str) -> String {
    format!("Role alias {name} not found")
}

/// One alias to create and then update: a label, the name, the region the calls are
/// signed for, the create's body, the update's body, and what the record must report
/// afterwards.
struct Case {
    label: &'static str,
    name: String,
    region: &'static str,
    create: Value,
    update: Value,
    expected_role_arn: &'static str,
    expected_duration: i64,
}

/// The table every case list below is built from. It spans what the rule generalises
/// over: which of the two optional update members the request carries, the duration's
/// position in the modelled range, the form and length of the name, the presence of
/// tags on the create, and the region.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "the role ARN alone, leaving the created duration in place",
            name: "role_arn_only".to_string(),
            region: REGION,
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/first",
                "credentialDurationSeconds": 1_800,
            }),
            update: json!({ "roleArn": "arn:aws:iam::000000000000:role/second" }),
            expected_role_arn: "arn:aws:iam::000000000000:role/second",
            expected_duration: 1_800,
        },
        Case {
            label: "the shortest duration the range allows, alone",
            name: "duration_only_min".to_string(),
            region: REGION,
            create: json!({ "roleArn": "arn:aws:iam::000000000000:role/kept" }),
            update: json!({ "credentialDurationSeconds": MIN_CREDENTIAL_DURATION_SECONDS }),
            expected_role_arn: "arn:aws:iam::000000000000:role/kept",
            expected_duration: MIN_CREDENTIAL_DURATION_SECONDS,
        },
        Case {
            label: "the longest duration the range allows, alone",
            name: "duration_only_max".to_string(),
            region: REGION,
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/kept",
                "credentialDurationSeconds": MIN_CREDENTIAL_DURATION_SECONDS,
            }),
            update: json!({ "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS }),
            expected_role_arn: "arn:aws:iam::000000000000:role/kept",
            expected_duration: MAX_CREDENTIAL_DURATION_SECONDS,
        },
        Case {
            label: "both members at once, on a name using every character class",
            name: "Alias_01=aZ,x@y-z".to_string(),
            region: REGION,
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/classes",
                "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS,
                "tags": [
                    { "Key": "team", "Value": "platform" },
                    { "Key": "env", "Value": "test" },
                ],
            }),
            update: json!({
                "roleArn": "arn:aws:iam::000000000000:role/rotated",
                "credentialDurationSeconds": 7_200,
            }),
            expected_role_arn: "arn:aws:iam::000000000000:role/rotated",
            expected_duration: 7_200,
        },
        Case {
            label: "neither member, on the shortest legal name in another region",
            name: "a".to_string(),
            region: "eu-west-1",
            create: json!({ "roleArn": "arn:aws:iam::000000000000:role/eu" }),
            update: json!({}),
            expected_role_arn: "arn:aws:iam::000000000000:role/eu",
            expected_duration: DEFAULT_CREDENTIAL_DURATION_SECONDS,
        },
        Case {
            label: "both members on the longest legal name in another region",
            name: "z".repeat(MAX_ROLE_ALIAS_LEN),
            region: "ap-southeast-2",
            create: json!({
                "roleArn": "arn:aws:iam::1:role/r",
                "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS,
            }),
            update: json!({
                "roleArn": "arn:aws:iam::000000000000:role/long",
                "credentialDurationSeconds": MIN_CREDENTIAL_DURATION_SECONDS,
            }),
            expected_role_arn: "arn:aws:iam::000000000000:role/long",
            expected_duration: MIN_CREDENTIAL_DURATION_SECONDS,
        },
    ]
}

#[tokio::test]
async fn an_update_rewrites_only_the_members_it_names() {
    for case in cases() {
        let label = case.label;
        // A fresh emulator per row, so what the update writes is what this row created.
        let emulator = Emulator::new();

        let created = emulator
            .create_in(&case.name, &case.create, case.region)
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateRoleAlias must succeed, got {created:?}"
        );
        let before = emulator.describe(&case.name).await;
        assert_eq!(
            before.status, 200,
            "case {label:?}: the created alias must be readable, got {before:?}"
        );

        let updated = emulator
            .update_in(&case.name, &case.update, case.region)
            .await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: UpdateRoleAlias must succeed, got {updated:?}"
        );
        // The update reports the alias it wrote and the ARN the CREATE minted: the name
        // and the ARN are the alias's identity, and no update member names either.
        assert_eq!(
            updated.text("roleAlias").as_deref(),
            Some(case.name.as_str()),
            "case {label:?}: the update must report the alias it wrote, got {updated:?}"
        );
        let expected_arn = role_alias_arn(case.region, &case.name);
        assert_eq!(
            updated.text("roleAliasArn"),
            Some(expected_arn.clone()),
            "case {label:?}: the update must report the ARN the create minted, got \
             {updated:?}"
        );

        let after = emulator.describe(&case.name).await;
        assert_eq!(
            after.status, 200,
            "case {label:?}: the updated alias must be readable, got {after:?}"
        );
        assert_eq!(
            after.described("roleArn").and_then(Value::as_str),
            Some(case.expected_role_arn),
            "case {label:?}: expected roleArn {:?}, got {after:?}",
            case.expected_role_arn
        );
        assert_eq!(
            after.described_number("credentialDurationSeconds"),
            Some(case.expected_duration as f64),
            "case {label:?}: expected credentialDurationSeconds {}, got {after:?}",
            case.expected_duration
        );
        // The identity and the members no update member names are what the create wrote.
        for unchanged in ["roleAlias", "roleAliasArn", "owner", "creationDate"] {
            assert_eq!(
                before.described(unchanged),
                after.described(unchanged),
                "case {label:?}: an update must leave {unchanged} as the create wrote it, \
                 got {after:?}"
            );
        }
        assert_eq!(
            after.described("roleAliasArn").and_then(Value::as_str),
            Some(expected_arn.as_str()),
            "case {label:?}: the record must keep the ARN its region minted, got {after:?}"
        );
        // "The UNIX timestamp of when the role alias was last modified" is the moment of
        // the write, which is never before the record was created.
        let created_at = after
            .described_number("creationDate")
            .unwrap_or_else(|| panic!("case {label:?}: the read reported no creationDate"));
        let modified_at = after
            .described_number("lastModifiedDate")
            .unwrap_or_else(|| panic!("case {label:?}: the read reported no lastModifiedDate"));
        assert!(
            modified_at >= created_at,
            "case {label:?}: expected lastModifiedDate at or after creationDate \
             ({created_at}), got {modified_at}"
        );

        // A read of the updated record changes nothing a later caller sees.
        let reread = emulator.describe(&case.name).await;
        assert_eq!(
            after.body, reread.body,
            "case {label:?}: re-reading the alias must report the same record, got \
             {reread:?}"
        );
    }
}

#[tokio::test]
async fn updating_one_role_alias_leaves_every_other_in_place() {
    let emulator = Emulator::new();

    // Every row lives in one store, so each update below is a write to one record out of
    // many rather than to the only record there is.
    for case in cases() {
        let label = case.label;
        let created = emulator
            .create_in(&case.name, &case.create, case.region)
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateRoleAlias must succeed, got {created:?}"
        );
    }

    for case in cases() {
        let label = case.label;
        // What every OTHER alias reports right before the update, so the comparison
        // after it is against a value this test read rather than one it rebuilt.
        let mut others = Vec::new();
        for other in cases() {
            if other.name != case.name {
                others.push((other.label, other.name.clone(), emulator.describe(&other.name).await));
            }
        }

        let updated = emulator
            .update_in(&case.name, &case.update, case.region)
            .await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: UpdateRoleAlias must succeed, got {updated:?}"
        );

        for (other_label, other_name, before) in others {
            let after = emulator.describe(&other_name).await;
            assert_eq!(
                before.body, after.body,
                "case {label:?}: updating it must leave {other_label:?} untouched, got \
                 {after:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_of_an_alias_the_store_does_not_hold_is_refused_not_found() {
    let emulator = Emulator::new();

    // One alias exists and stays, so the refusals below are about the NAME not being in
    // the store rather than the store being empty.
    let created = emulator
        .create(
            "existing_alias",
            &json!({ "roleArn": "arn:aws:iam::000000000000:role/kept" }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the setup CreateRoleAlias must succeed, got {created:?}"
    );
    let kept_before = emulator.describe("existing_alias").await;

    // An alias created and then DELETED: an update of it is refused for exactly the same
    // reason a name nobody ever created is — the account holds no alias under it — so a
    // client can tell "this alias is gone, stop" from a server fault.
    let created = emulator
        .create(
            "deleted_alias",
            &json!({
                "roleArn": "arn:aws:iam::000000000000:role/gone",
                "credentialDurationSeconds": 3_600,
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the setup CreateRoleAlias must succeed, got {created:?}"
    );
    let deleted = emulator.delete("deleted_alias").await;
    assert_eq!(
        deleted.status, 200,
        "the setup DeleteRoleAlias must succeed, got {deleted:?}"
    );

    // The names differ in form, length and case — a role alias name is case-sensitive,
    // so a name that differs from an existing one only in case names no alias — and the
    // bodies differ in which members they carry, since none of them is reached.
    let bodies = [
        json!({}),
        json!({ "credentialDurationSeconds": 1_800 }),
        json!({ "roleArn": "arn:aws:iam::000000000000:role/other" }),
        json!({
            "roleArn": "arn:aws:iam::000000000000:role/other",
            "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS,
        }),
    ];
    let names = [
        "a".to_string(),
        "deleted_alias".to_string(),
        "unknown_alias".to_string(),
        "existing_alias_2".to_string(),
        "EXISTING_ALIAS".to_string(),
        "Missing_Alias=1,2@host-9".to_string(),
        "n".repeat(MAX_ROLE_ALIAS_LEN),
    ];
    for name in &names {
        for body in &bodies {
            let refused = emulator.update(name, body).await;
            assert_eq!(
                refused.status, 404,
                "case {name:?}/{body}: an update of a name the account holds no alias \
                 under must be refused 404, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("ResourceNotFoundException"),
                "case {name:?}/{body}: expected ResourceNotFoundException, got {refused:?}"
            );
            // The refusal is the family's sentence with the name taken from the request,
            // and it is the SAME sentence the family's reader refuses a miss with.
            assert_eq!(
                refused.text("message"),
                Some(missing_message(name)),
                "case {name:?}/{body}: unexpected not-found message, got {refused:?}"
            );
            assert_eq!(
                refused.text("message"),
                emulator.describe(name).await.text("message"),
                "case {name:?}/{body}: the update and the read must word the miss the \
                 same way, got {refused:?}"
            );
            // A refused update records nothing: the name is still free.
            let read = emulator.describe(name).await;
            assert_eq!(
                read.status, 404,
                "case {name:?}/{body}: a refused update must create nothing, got {read:?}"
            );
        }
    }

    // The refused updates changed nothing: the alias that was there is untouched.
    let kept_after = emulator.describe("existing_alias").await;
    assert_eq!(
        kept_before.body, kept_after.body,
        "a refused update must leave the existing alias untouched, got {kept_after:?}"
    );
}

#[tokio::test]
async fn an_update_whose_duration_is_outside_the_modelled_range_is_refused() {
    let emulator = Emulator::new();

    // `credentialDurationSeconds` is modelled `range 900..43200`, so each published
    // bound is accepted and a value one step past either is refused 400
    // `ValidationException`, leaving the record exactly as it was.
    let created = emulator
        .create(
            "ranged_alias",
            &json!({
                "roleArn": "arn:aws:iam::000000000000:role/ranged",
                "credentialDurationSeconds": 1_800,
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the setup CreateRoleAlias must succeed, got {created:?}"
    );

    for duration in [
        MIN_CREDENTIAL_DURATION_SECONDS,
        MAX_CREDENTIAL_DURATION_SECONDS,
    ] {
        let accepted = emulator
            .update(
                "ranged_alias",
                &json!({ "credentialDurationSeconds": duration }),
            )
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {duration}: a published bound must be accepted, got {accepted:?}"
        );
        let read = emulator.describe("ranged_alias").await;
        assert_eq!(
            read.described_number("credentialDurationSeconds"),
            Some(duration as f64),
            "case {duration}: the bound must be persisted, got {read:?}"
        );
    }

    let before = emulator.describe("ranged_alias").await;
    for duration in [
        0,
        -1,
        MIN_CREDENTIAL_DURATION_SECONDS - 1,
        MAX_CREDENTIAL_DURATION_SECONDS + 1,
        86_400,
    ] {
        let refused = emulator
            .update(
                "ranged_alias",
                &json!({ "credentialDurationSeconds": duration }),
            )
            .await;
        assert_eq!(
            refused.status, 400,
            "case {duration}: a duration outside the modelled range must be refused 400, \
             got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case {duration}: expected this service's own validation-refusal class, got \
             {refused:?}"
        );
        let after = emulator.describe("ranged_alias").await;
        assert_eq!(
            before.body, after.body,
            "case {duration}: a refused update must write nothing, got {after:?}"
        );
    }
}

/// One step of a run of updates: the body the request carries, and the record the alias
/// must report once it has been applied.
struct Step {
    update: Value,
    expected_role_arn: &'static str,
    expected_duration: i64,
}

/// One create followed by a RUN of updates, which is what "replaces exactly the members
/// the request carried" means over more than one call.
struct SequenceCase {
    label: &'static str,
    name: &'static str,
    region: &'static str,
    create: Value,
    steps: Vec<Step>,
}

/// The runs the fold is asserted over. They span the length of the run, which subset of
/// the two optional members each step names, the order the members are named in, a step
/// repeated unchanged, the duration's position in the modelled range, the form and length
/// of the name, whether the create carried tags, and the region.
fn sequence_cases() -> Vec<SequenceCase> {
    vec![
        SequenceCase {
            label: "the role ARN replaced twice, then a step naming neither member",
            name: "arn_then_arn_then_none",
            region: REGION,
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/first",
                "credentialDurationSeconds": 1_800,
            }),
            steps: vec![
                Step {
                    update: json!({ "roleArn": "arn:aws:iam::000000000000:role/second" }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/second",
                    expected_duration: 1_800,
                },
                Step {
                    update: json!({ "roleArn": "arn:aws:iam::000000000000:role/third" }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/third",
                    expected_duration: 1_800,
                },
                Step {
                    update: json!({}),
                    expected_role_arn: "arn:aws:iam::000000000000:role/third",
                    expected_duration: 1_800,
                },
            ],
        },
        SequenceCase {
            label: "each member in turn, then both at once, from a create naming no duration",
            name: "duration_then_arn_then_both",
            region: REGION,
            create: json!({ "roleArn": "arn:aws:iam::000000000000:role/first" }),
            steps: vec![
                Step {
                    update: json!({ "credentialDurationSeconds": MIN_CREDENTIAL_DURATION_SECONDS }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/first",
                    expected_duration: MIN_CREDENTIAL_DURATION_SECONDS,
                },
                Step {
                    update: json!({ "roleArn": "arn:aws:iam::000000000000:role/second" }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/second",
                    expected_duration: MIN_CREDENTIAL_DURATION_SECONDS,
                },
                Step {
                    update: json!({
                        "roleArn": "arn:aws:iam::000000000000:role/third",
                        "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS,
                    }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/third",
                    expected_duration: MAX_CREDENTIAL_DURATION_SECONDS,
                },
            ],
        },
        SequenceCase {
            label: "the duration walked across the range while the ARN is named once",
            name: "Duration_Walk=1,2@host-9",
            region: "eu-west-1",
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/first",
                "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS,
            }),
            steps: vec![
                Step {
                    update: json!({ "credentialDurationSeconds": MIN_CREDENTIAL_DURATION_SECONDS }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/first",
                    expected_duration: MIN_CREDENTIAL_DURATION_SECONDS,
                },
                Step {
                    update: json!({ "roleArn": "arn:aws:iam::000000000000:role/second" }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/second",
                    expected_duration: MIN_CREDENTIAL_DURATION_SECONDS,
                },
                Step {
                    update: json!({ "credentialDurationSeconds": 21_600 }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/second",
                    expected_duration: 21_600,
                },
                Step {
                    update: json!({ "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/second",
                    expected_duration: MAX_CREDENTIAL_DURATION_SECONDS,
                },
            ],
        },
        SequenceCase {
            label: "the same update repeated three times over a tagged alias",
            name: "repeated_update",
            region: "ap-southeast-2",
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/first",
                "credentialDurationSeconds": 3_600,
                "tags": [{ "Key": "team", "Value": "platform" }],
            }),
            steps: (0..3)
                .map(|_| Step {
                    update: json!({
                        "roleArn": "arn:aws:iam::000000000000:role/second",
                        "credentialDurationSeconds": 7_200,
                    }),
                    expected_role_arn: "arn:aws:iam::000000000000:role/second",
                    expected_duration: 7_200,
                })
                .collect(),
        },
        SequenceCase {
            label: "nothing but steps naming neither member, on the shortest legal name",
            name: "a",
            region: REGION,
            create: json!({ "roleArn": "arn:aws:iam::000000000000:role/first" }),
            steps: vec![
                Step {
                    update: json!({}),
                    expected_role_arn: "arn:aws:iam::000000000000:role/first",
                    expected_duration: DEFAULT_CREDENTIAL_DURATION_SECONDS,
                },
                Step {
                    update: json!({}),
                    expected_role_arn: "arn:aws:iam::000000000000:role/first",
                    expected_duration: DEFAULT_CREDENTIAL_DURATION_SECONDS,
                },
            ],
        },
    ]
}

#[tokio::test]
async fn a_run_of_updates_leaves_the_latest_value_named_for_each_member() {
    for case in sequence_cases() {
        let label = case.label;
        let name = case.name;
        // A fresh emulator per row, so the run below is applied to the record this row
        // created.
        let emulator = Emulator::new();

        let created = emulator.create_in(name, &case.create, case.region).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateRoleAlias must succeed, got {created:?}"
        );
        let before = emulator.describe(name).await;
        assert_eq!(
            before.status, 200,
            "case {label:?}: the created alias must be readable, got {before:?}"
        );
        let expected_arn = role_alias_arn(case.region, name);

        // The moment the record was created: every step's last-modified stamp is at or
        // after it, since the record cannot have been modified before it existed.
        let created_at = before
            .described_number("creationDate")
            .unwrap_or_else(|| panic!("case {label:?}: the create reported no creationDate"));
        // The stamp every step must leave at or after where the previous one left it:
        // "the UNIX timestamp of when the role alias was last modified" moves forward
        // with each write and never backwards.
        let mut previous_modified_at = before
            .described_number("lastModifiedDate")
            .unwrap_or_else(|| panic!("case {label:?}: the create reported no lastModifiedDate"));

        for (index, step) in case.steps.iter().enumerate() {
            let step_label = format!("{label} (step {index})");
            let updated = emulator.update_in(name, &step.update, case.region).await;
            assert_eq!(
                updated.status, 200,
                "case {step_label:?}: UpdateRoleAlias must succeed, got {updated:?}"
            );
            assert_eq!(
                updated.text("roleAlias").as_deref(),
                Some(name),
                "case {step_label:?}: the update must report the alias it wrote, got \
                 {updated:?}"
            );
            assert_eq!(
                updated.text("roleAliasArn"),
                Some(expected_arn.clone()),
                "case {step_label:?}: the update must report the ARN the create minted, \
                 got {updated:?}"
            );

            let after = emulator.describe(name).await;
            assert_eq!(
                after.status, 200,
                "case {step_label:?}: the updated alias must be readable, got {after:?}"
            );
            assert_eq!(
                after.described("roleArn").and_then(Value::as_str),
                Some(step.expected_role_arn),
                "case {step_label:?}: expected roleArn {:?}, got {after:?}",
                step.expected_role_arn
            );
            assert_eq!(
                after.described_number("credentialDurationSeconds"),
                Some(step.expected_duration as f64),
                "case {step_label:?}: expected credentialDurationSeconds {}, got {after:?}",
                step.expected_duration
            );
            // The alias's identity and its owner are not update members, so however many
            // updates ran, they are still what the create wrote.
            for unchanged in ["roleAlias", "roleAliasArn", "owner", "creationDate"] {
                assert_eq!(
                    before.described(unchanged),
                    after.described(unchanged),
                    "case {step_label:?}: a run of updates must leave {unchanged} as the \
                     create wrote it, got {after:?}"
                );
            }

            let modified_at = after.described_number("lastModifiedDate").unwrap_or_else(|| {
                panic!("case {step_label:?}: the read reported no lastModifiedDate")
            });
            assert!(
                modified_at >= previous_modified_at,
                "case {step_label:?}: expected lastModifiedDate at or after the previous \
                 write's ({previous_modified_at}), got {modified_at}"
            );
            assert!(
                modified_at >= created_at,
                "case {step_label:?}: expected lastModifiedDate at or after creationDate \
                 ({created_at}), got {modified_at}"
            );
            previous_modified_at = modified_at;
        }
    }
}

/// One alias created with a tag set, to be updated: a label, the name, the `tags` the
/// create carried, and the tag set the alias must still report after the update.
struct TagCase {
    label: &'static str,
    name: &'static str,
    create: Value,
    update: Value,
    expected_tags: Vec<(&'static str, Option<&'static str>)>,
}

/// The tag sets the "an update replaces only the members it names" rule is asserted over
/// for a member NO `UpdateRoleAlias` request can name. They span the size of the tag set
/// (none, one, several), a tag carrying no value, and which update members the request
/// carries.
fn tag_cases() -> Vec<TagCase> {
    vec![
        TagCase {
            label: "an alias the create tagged with nothing",
            name: "untagged",
            create: json!({ "roleArn": "arn:aws:iam::000000000000:role/first" }),
            update: json!({
                "roleArn": "arn:aws:iam::000000000000:role/second",
                "credentialDurationSeconds": 7_200,
            }),
            expected_tags: vec![],
        },
        TagCase {
            label: "an alias carrying one tag, updated on its duration alone",
            name: "one_tag",
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/first",
                "tags": [{ "Key": "team", "Value": "platform" }],
            }),
            update: json!({ "credentialDurationSeconds": MAX_CREDENTIAL_DURATION_SECONDS }),
            expected_tags: vec![("team", Some("platform"))],
        },
        TagCase {
            label: "an alias carrying several tags, one of them valueless",
            name: "several_tags",
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/first",
                "credentialDurationSeconds": MIN_CREDENTIAL_DURATION_SECONDS,
                "tags": [
                    { "Key": "env", "Value": "test" },
                    { "Key": "owner", "Value": "devices" },
                    { "Key": "marker" },
                ],
            }),
            update: json!({ "roleArn": "arn:aws:iam::000000000000:role/second" }),
            expected_tags: vec![
                ("env", Some("test")),
                ("marker", None),
                ("owner", Some("devices")),
            ],
        },
        TagCase {
            label: "an alias carrying one tag, updated on neither member",
            name: "no_op_update",
            create: json!({
                "roleArn": "arn:aws:iam::000000000000:role/first",
                "tags": [{ "Key": "env", "Value": "test" }],
            }),
            update: json!({}),
            expected_tags: vec![("env", Some("test"))],
        },
    ]
}

#[tokio::test]
async fn an_update_leaves_the_tags_the_create_wrote_in_place() {
    for case in tag_cases() {
        let label = case.label;
        let name = case.name;
        let emulator = Emulator::new();

        let created = emulator.create(name, &case.create).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateRoleAlias must succeed, got {created:?}"
        );

        let arn = role_alias_arn(REGION, name);
        let expected: BTreeMap<String, Option<String>> = case
            .expected_tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.map(str::to_string)))
            .collect();

        let before = emulator.list_tags(&arn, REGION).await;
        assert_eq!(
            before.status, 200,
            "case {label:?}: ListTagsForResource must succeed, got {before:?}"
        );
        assert_eq!(
            before.tags().unwrap_or_default(),
            expected,
            "case {label:?}: expected the created tag set {expected:?}, got {before:?}"
        );

        let updated = emulator.update(name, &case.update).await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: UpdateRoleAlias must succeed, got {updated:?}"
        );

        // `tags` is not an `UpdateRoleAlias` member, so an update can neither add, drop
        // nor rewrite one: the tag set is still what the create wrote.
        let after = emulator.list_tags(&arn, REGION).await;
        assert_eq!(
            after.status, 200,
            "case {label:?}: ListTagsForResource must succeed after the update, got \
             {after:?}"
        );
        assert_eq!(
            after.tags().unwrap_or_default(),
            expected,
            "case {label:?}: an update must leave the tag set {expected:?} in place, got \
             {after:?}"
        );
    }
}

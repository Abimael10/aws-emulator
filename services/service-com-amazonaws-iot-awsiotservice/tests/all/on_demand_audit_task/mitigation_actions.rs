// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of DEVICE DEFENDER MITIGATION ACTIONS:
//! `CreateMitigationAction`, the `DescribeMitigationAction` and `ListMitigationActions`
//! readers that prove what it persisted, `UpdateMitigationAction` and
//! `DeleteMitigationAction`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a mitigation action is a named, account-scoped record. A create
//! persists, under the name the request line carries, the IAM role it names, the ONE type
//! of change its `actionParams` define and the tags it carries; it mints an id and the ARN
//! `arn:aws:iot:<region>:<account>:mitigationaction/<actionName>` for it, and every later
//! read of that name reports THAT record — `actionType` derived from the stored
//! parameters, and the creation and last-modified instants — until an update replaces the
//! members it names (keeping the name, the id and the ARN) or a delete removes it. A name
//! the account does not hold is `ResourceNotFoundException` (404) to the readers and to
//! the update, a name it already holds is `ResourceAlreadyExistsException` (409) to the
//! create, and parameters that define no single type are `InvalidRequestException` (400).
//! `DeleteMitigationAction` models no not-found error at all, so a repeated delete answers
//! exactly as the first one did.
//!
//! The table spans what that rule generalises over: every modelled `MitigationActionType`,
//! which is one per member of `MitigationActionParams`
//! (`smithy_query(query="shape",shape="MitigationActionParams")`) — read against the
//! generated `MITIGATION_ACTION_TYPE_VALUES` so a type the model gains is a row this suite
//! demands — plus the page size and the `actionType` filter a listing is taken under.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::ops::types::MITIGATION_ACTION_TYPE_VALUES;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "eu-central-1";
/// The role a mitigation action is applied with: its form is the caller's, so one
/// well-formed ARN is used throughout.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-mitigation";
/// A second well-formed role ARN: for the update that names only `roleArn`, and for the
/// refused create that must not replace the stored record's role.
const REPLACEMENT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/other-mitigation";

/// One row of the table: the action's name, the parameters it is created with, and the
/// `MitigationActionType` those parameters define.
///
/// One row per member of `MitigationActionParams`, which is one per modelled
/// `MitigationActionType`; each member's own required values are the ones its shape
/// declares (`DEACTIVATE` for either certificate action, `BLANK_POLICY` for the policy
/// template, a `LogLevel` for IoT logging).
fn rows() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "deactivate-device-certificate",
            json!({ "updateDeviceCertificateParams": { "action": "DEACTIVATE" } }),
            "UPDATE_DEVICE_CERTIFICATE",
        ),
        (
            "deactivate-ca-certificate",
            json!({ "updateCACertificateParams": { "action": "DEACTIVATE" } }),
            "UPDATE_CA_CERTIFICATE",
        ),
        (
            "quarantine_things",
            json!({
                "addThingsToThingGroupParams": {
                    "thingGroupNames": ["quarantine", "under-review"],
                    "overrideDynamicGroups": true,
                },
            }),
            "ADD_THINGS_TO_THING_GROUP",
        ),
        (
            "blank-the-policy",
            json!({ "replaceDefaultPolicyVersionParams": { "templateName": "BLANK_POLICY" } }),
            "REPLACE_DEFAULT_POLICY_VERSION",
        ),
        (
            "turn-logging-on",
            json!({
                "enableIoTLoggingParams": {
                    "roleArnForLogging": "arn:aws:iam::000000000000:role/iot-logging",
                    "logLevel": "WARN",
                },
            }),
            "ENABLE_IOT_LOGGING",
        ),
        (
            "tell-the-topic",
            json!({
                "publishFindingToSnsParams": {
                    "topicArn": "arn:aws:sns:eu-central-1:000000000000:defender-findings",
                },
            }),
            "PUBLISH_FINDING_TO_SNS",
        ),
    ]
}

/// The ARN this plane mints for `action_name`.
fn expected_arn(action_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:mitigationaction/{action_name}")
}

/// The parameters of two rows in one `actionParams`, which defines two types of change and
/// so defines no action at all.
fn two_types(first: &Value, second: &Value) -> Value {
    let mut both = first.clone();
    let members = both
        .as_object_mut()
        .expect("a row's parameters are a JSON object");
    for (member, value) in second
        .as_object()
        .expect("a row's parameters are a JSON object")
    {
        members.insert(member.clone(), value.clone());
    }
    both
}

/// One emulator: the shared engine plus a router with this service mounted.
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

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the mitigation-action request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create(&self, action_name: &str, body: &Value) -> Reply {
        self.dispatch(
            "POST",
            &format!("/mitigationactions/actions/{action_name}"),
            Some(body),
        )
        .await
    }

    async fn describe(&self, action_name: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/mitigationactions/actions/{action_name}"),
            None,
        )
        .await
    }

    async fn update(&self, action_name: &str, body: &Value) -> Reply {
        self.dispatch(
            "PATCH",
            &format!("/mitigationactions/actions/{action_name}"),
            Some(body),
        )
        .await
    }

    async fn delete(&self, action_name: &str) -> Reply {
        self.dispatch(
            "DELETE",
            &format!("/mitigationactions/actions/{action_name}"),
            None,
        )
        .await
    }

    async fn list(&self, query: &str) -> Reply {
        self.dispatch("GET", &format!("/mitigationactions/actions{query}"), None)
            .await
    }

    /// Create every row of the table, asserting each create reports the identity it minted,
    /// and answer the ids it minted in the table's order.
    async fn create_every_row(&self) -> Vec<String> {
        let mut minted = Vec::new();
        for (action_name, params, _action_type) in rows() {
            let created = self
                .create(
                    action_name,
                    &json!({ "roleArn": ROLE_ARN, "actionParams": params }),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {action_name}: expected 200 from the create, got {} body {}",
                created.status,
                created.text()
            );
            assert_eq!(
                created.json()["actionArn"].as_str(),
                Some(expected_arn(action_name).as_str()),
                "case {action_name}: expected the ARN this plane mints, got {}",
                created.text()
            );
            let action_id = created.json()["actionId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                !action_id.is_empty(),
                "case {action_name}: expected a minted actionId, got {}",
                created.text()
            );
            minted.push(action_id);
        }
        minted
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }

    /// The names `ListMitigationActions` reported, in the order it reported them.
    fn listed_names(&self) -> Vec<String> {
        self.json()["actionIdentifiers"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["actionName"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The table covers the whole modelled catalog, so a `MitigationActionType` the model
/// gains is a row this suite demands rather than a case nothing exercises.
#[test]
fn the_table_spans_every_modeled_mitigation_action_type() {
    let covered: BTreeSet<&str> = rows()
        .into_iter()
        .map(|(_name, _params, action_type)| action_type)
        .collect();
    let modeled: BTreeSet<&str> = MITIGATION_ACTION_TYPE_VALUES.iter().copied().collect();
    assert_eq!(
        covered, modeled,
        "expected one row per modelled MitigationActionType {MITIGATION_ACTION_TYPE_VALUES:?}, \
         got {covered:?}"
    );
}

/// RULE: a created action is read back member for member, with the type its parameters
/// define, whichever type that is.
#[tokio::test]
async fn a_created_mitigation_action_is_read_back_member_for_member() {
    let emulator = Emulator::new();
    let minted = emulator.create_every_row().await;

    // Each action carries an id of its own: the member is "a unique identifier for the new
    // mitigation action", so two creates cannot report one id.
    let distinct: BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected one distinct actionId per action, got {minted:?}"
    );

    for ((action_name, params, action_type), action_id) in rows().into_iter().zip(minted.iter()) {
        let read = emulator.describe(action_name).await;
        assert_eq!(
            read.status,
            200,
            "case {action_name}: expected 200 from the read, got {} body {}",
            read.status,
            read.text()
        );
        let described = read.json();
        assert_eq!(
            (
                described["actionName"].as_str(),
                described["actionType"].as_str(),
                described["actionArn"].as_str(),
                described["actionId"].as_str(),
                described["roleArn"].as_str(),
            ),
            (
                Some(action_name),
                Some(action_type),
                Some(expected_arn(action_name).as_str()),
                Some(action_id.as_str()),
                Some(ROLE_ARN),
            ),
            "case {action_name}: expected the record the create persisted, with the type its \
             parameters define, got {described}"
        );
        assert_eq!(
            described["actionParams"], params,
            "case {action_name}: expected the parameters the create carried, got {described}"
        );
        // A record that has had one write carries one instant: its create's.
        assert_eq!(
            described["creationDate"].as_f64(),
            described["lastModifiedDate"].as_f64(),
            "case {action_name}: expected a freshly created action to report one instant for \
             both dates, got {described}"
        );
        assert!(
            described["creationDate"].as_f64().unwrap_or_default() > 0.0,
            "case {action_name}: expected a creationDate, got {described}"
        );
    }
}

/// RULE: the listing is a page of the account's records in a stable order, filtered by the
/// type the stored parameters define, and a token resumes it where the last page ended.
#[tokio::test]
async fn mitigation_actions_are_listed_filtered_and_paged_over_their_records() {
    let emulator = Emulator::new();
    emulator.create_every_row().await;
    let every_name: BTreeSet<String> = rows()
        .into_iter()
        .map(|(action_name, _params, _action_type)| action_name.to_string())
        .collect();

    // One page at a time, with the smallest page that still has a boundary: `maxResults`
    // bounds a page and the token resumes the listing after the row the last page ended
    // on, so walking the pages yields every record exactly once.
    let page_size = 2;
    let mut seen: Vec<String> = Vec::new();
    let mut query = format!("?maxResults={page_size}");
    for page in 0..=every_name.len() {
        let listed = emulator.list(&query).await;
        assert_eq!(
            listed.status,
            200,
            "case page {page}: expected 200 from the listing, got {} body {}",
            listed.status,
            listed.text()
        );
        let names = listed.listed_names();
        assert!(
            names.len() <= page_size,
            "case page {page}: expected at most {page_size} rows, got {}",
            listed.text()
        );
        for identifier in listed.json()["actionIdentifiers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let action_name = identifier["actionName"].as_str().unwrap_or_default();
            assert_eq!(
                identifier["actionArn"].as_str(),
                Some(expected_arn(action_name).as_str()),
                "case page {page}: expected each row to carry the record's ARN, got {identifier}"
            );
            assert!(
                identifier["creationDate"].as_f64().unwrap_or_default() > 0.0,
                "case page {page}: expected each row to carry its creationDate, got {identifier}"
            );
        }
        seen.extend(names);
        match listed.json()["nextToken"].as_str() {
            Some(token) => query = format!("?maxResults={page_size}&nextToken={token}"),
            None => break,
        }
    }
    assert_eq!(
        seen.iter().cloned().collect::<BTreeSet<String>>(),
        every_name,
        "expected the pages together to hold every created action, got {seen:?}"
    );
    assert_eq!(
        seen.len(),
        every_name.len(),
        "expected no action on two pages, got {seen:?}"
    );
    // The order is the store's own, which is what makes a page boundary meaningful.
    let mut ascending = seen.clone();
    ascending.sort();
    assert_eq!(
        seen, ascending,
        "expected the listing to page over a stable ascending order, got {seen:?}"
    );

    // `actionType` "limit[s] the result to mitigation actions with a specific action type",
    // and the type is the one the stored parameters define.
    for (action_name, _params, action_type) in rows() {
        let filtered = emulator.list(&format!("?actionType={action_type}")).await;
        assert_eq!(
            filtered.status,
            200,
            "case {action_type}: expected 200 from the filtered listing, got {} body {}",
            filtered.status,
            filtered.text()
        );
        assert_eq!(
            filtered.listed_names(),
            vec![action_name.to_string()],
            "case {action_type}: expected only the action whose parameters define that type, \
             got {}",
            filtered.text()
        );
    }

    // A token this listing never issued selects no page of it.
    let refused = emulator
        .list("?nextToken=not-a-token-this-listing-issued")
        .await;
    assert_eq!(
        refused.status,
        400,
        "case a foreign nextToken: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "InvalidRequestException",
        "case a foreign nextToken: got {}",
        refused.text()
    );
}

/// RULE: an update replaces the members it names and keeps the rest, including the identity
/// minted on create; a delete removes the record, and models no not-found error.
#[tokio::test]
async fn an_update_replaces_the_members_it_names_and_a_delete_is_idempotent() {
    let emulator = Emulator::new();
    let minted = emulator.create_every_row().await;
    let table = rows();
    let (action_name, created_params, created_type) = table[0].clone();
    let (_other_name, other_params, other_type) = table[1].clone();
    let action_id = minted[0].clone();
    let creation_date = emulator.describe(action_name).await.json()["creationDate"]
        .as_f64()
        .unwrap_or_default();

    // `roleArn` alone: the role is replaced, the parameters and the identity minted on
    // create are not, and the record's last-modified instant is at or after the instant it
    // was created at.
    let role_only = emulator
        .update(action_name, &json!({ "roleArn": REPLACEMENT_ROLE_ARN }))
        .await;
    assert_eq!(
        role_only.status,
        200,
        "case updating roleArn alone: expected 200, got {} body {}",
        role_only.status,
        role_only.text()
    );
    assert_eq!(
        (
            role_only.json()["actionArn"].as_str(),
            role_only.json()["actionId"].as_str()
        ),
        (
            Some(expected_arn(action_name).as_str()),
            Some(action_id.as_str())
        ),
        "case updating roleArn alone: expected the identity minted on create, got {}",
        role_only.text()
    );
    let described = emulator.describe(action_name).await.json();
    assert_eq!(
        (
            described["roleArn"].as_str(),
            described["actionType"].as_str()
        ),
        (Some(REPLACEMENT_ROLE_ARN), Some(created_type)),
        "case updating roleArn alone: expected the new role and the stored type, got {described}"
    );
    assert_eq!(
        described["actionParams"], created_params,
        "case updating roleArn alone: expected the stored parameters to be kept, got {described}"
    );
    assert_eq!(
        described["creationDate"].as_f64(),
        Some(creation_date),
        "case updating roleArn alone: expected the creationDate to stand, got {described}"
    );
    assert!(
        described["lastModifiedDate"].as_f64().unwrap_or_default() >= creation_date,
        "case updating roleArn alone: expected lastModifiedDate at or after creationDate, got \
         {described}"
    );

    // `actionParams` alone: the type an action applies is the type its stored parameters
    // define, so replacing them replaces the reported type, and the role the previous
    // update left stands.
    let params_only = emulator
        .update(action_name, &json!({ "actionParams": other_params }))
        .await;
    assert_eq!(
        params_only.status,
        200,
        "case updating actionParams alone: expected 200, got {} body {}",
        params_only.status,
        params_only.text()
    );
    let described = emulator.describe(action_name).await.json();
    assert_eq!(
        (
            described["actionType"].as_str(),
            described["roleArn"].as_str()
        ),
        (Some(other_type), Some(REPLACEMENT_ROLE_ARN)),
        "case updating actionParams alone: expected the type the new parameters define and the \
         stored role, got {described}"
    );
    assert_eq!(
        described["actionParams"], other_params,
        "case updating actionParams alone: expected the new parameters, got {described}"
    );

    // The delete removes the record, and a repeat of it answers exactly as the first did:
    // this operation models no not-found error at all.
    for attempt in ["the first delete", "a repeated delete"] {
        let deleted = emulator.delete(action_name).await;
        assert_eq!(
            deleted.status,
            200,
            "case {attempt}: expected 200, got {} body {}",
            deleted.status,
            deleted.text()
        );
        let missing = emulator.describe(action_name).await;
        assert_eq!(
            missing.status,
            404,
            "case {attempt}: expected the read of a deleted action to be 404, got {} body {}",
            missing.status,
            missing.text()
        );
        assert_eq!(
            missing.error_code(),
            "ResourceNotFoundException",
            "case {attempt}: got {}",
            missing.text()
        );
        assert_eq!(
            missing.error_message(),
            format!("No action with name {action_name} is found"),
            "case {attempt}: expected the sentence a reader states the miss with, got {}",
            missing.text()
        );
    }

    // Only that record is gone: the listing still holds every other row.
    let listed = emulator.list("?maxResults=250").await;
    let expected: Vec<String> = {
        let mut names: Vec<String> = table
            .iter()
            .map(|(name, _params, _action_type)| (*name).to_string())
            .filter(|name| name != action_name)
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        listed.listed_names(),
        expected,
        "case after the delete: expected every other action to stand, got {}",
        listed.text()
    );
}

/// RULE: a request the store cannot take is refused with the modelled error, and the store
/// is left exactly as it was.
#[tokio::test]
async fn a_mitigation_action_the_store_cannot_take_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();
    let table = rows();
    let (action_name, params, action_type) = table[0].clone();
    let (_other_name, other_params, _other_type) = table[1].clone();

    // "Each mitigation action can apply only one type of change": parameters defining none
    // and parameters defining two are both refused — one row per side of that rule.
    for (case, bad_params) in [
        ("no type", json!({})),
        ("two types", two_types(&params, &other_params)),
    ] {
        let refused = emulator
            .create(
                "refused-action",
                &json!({ "roleArn": ROLE_ARN, "actionParams": bad_params }),
            )
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {case}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case}: got {}",
            refused.text()
        );
        // Nothing is created for a refused create.
        let missing = emulator.describe("refused-action").await;
        assert_eq!(
            missing.status,
            404,
            "case {case}: expected the refused create to leave no action behind, got {} body {}",
            missing.status,
            missing.text()
        );
    }

    // A name the account holds no action under: both the reader and the update refuse it
    // 404, each with the sentence its own path states the miss with.
    for (case, reply, expected_message) in [
        (
            "reading an action nothing created",
            emulator.describe("absent-action").await,
            "No action with name absent-action is found",
        ),
        (
            "updating an action nothing created",
            emulator
                .update("absent-action", &json!({ "roleArn": ROLE_ARN }))
                .await,
            "Resource findingActionName=absent-action was not found",
        ),
    ] {
        assert_eq!(
            reply.status,
            404,
            "case {case}: expected 404, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "ResourceNotFoundException",
            "case {case}: got {}",
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            expected_message,
            "case {case}: expected the sentence this path states the miss with, got {}",
            reply.text()
        );
    }

    // A name already taken is a conflict, and the stored record is left as it was: the
    // duplicate asks for a different role and different parameters.
    let created = emulator
        .create(
            action_name,
            &json!({
                "roleArn": ROLE_ARN,
                "actionParams": params,
                "tags": [{ "Key": "owner", "Value": "defender" }],
            }),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case the first create: expected 200, got {} body {}",
        created.status,
        created.text()
    );
    let duplicate = emulator
        .create(
            action_name,
            &json!({ "roleArn": REPLACEMENT_ROLE_ARN, "actionParams": other_params }),
        )
        .await;
    assert_eq!(
        duplicate.status,
        409,
        "case a name already taken: expected 409, got {} body {}",
        duplicate.status,
        duplicate.text()
    );
    assert_eq!(
        duplicate.error_code(),
        "ResourceAlreadyExistsException",
        "case a name already taken: got {}",
        duplicate.text()
    );
    assert_eq!(
        duplicate.error_message(),
        format!("MitigationAction cannot be created - name already exists (name={action_name})"),
        "case a name already taken: expected this family's already-exists sentence, got {}",
        duplicate.text()
    );
    assert_eq!(
        (
            duplicate.json()["resourceId"].as_str(),
            duplicate.json()["resourceArn"].as_str()
        ),
        (
            created.json()["actionId"].as_str(),
            Some(expected_arn(action_name).as_str())
        ),
        "case a name already taken: expected the refusal to report the record that holds the \
         name, got {}",
        duplicate.text()
    );
    let described = emulator.describe(action_name).await.json();
    assert_eq!(
        (
            described["roleArn"].as_str(),
            described["actionType"].as_str()
        ),
        (Some(ROLE_ARN), Some(action_type)),
        "case a name already taken: expected the stored action to be left as it was, got \
         {described}"
    );

    // An update whose parameters define two types is refused for the same reason a create
    // is, and leaves the stored parameters alone.
    let refused = emulator
        .update(
            action_name,
            &json!({ "actionParams": two_types(&params, &other_params) }),
        )
        .await;
    assert_eq!(
        refused.status,
        400,
        "case an update defining two types: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "InvalidRequestException",
        "case an update defining two types: got {}",
        refused.text()
    );
    let described = emulator.describe(action_name).await.json();
    assert_eq!(
        described["actionParams"], params,
        "case an update defining two types: expected the stored parameters to stand, got \
         {described}"
    );
}

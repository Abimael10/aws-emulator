// SPDX-License-Identifier: CC0-1.0

//! The origination-identity + send-text slice of `sms-voice`, end to end over the wire.
//!
//! Every request goes through [`MultiServiceRouter::dispatch`] as `awsJson1_0` — `POST /`
//! with `X-Amz-Target: PinpointSMSVoiceV2.<Op>` — so these tests exercise routing,
//! deserialization, constraint validation, the handler and serialization, exactly as a
//! real SDK client would reach them.
//!
//! The rules under test:
//!
//! * a create PERSISTS, so the matching describe/release reports the stored record and a
//!   release makes the resource absent;
//! * a resource name is unique per account: a second create of the same name conflicts
//!   and a second delete is not found;
//! * `DescribePhoneNumbers` narrows by id, by filter and by page, over the store's
//!   stable order;
//! * `DescribeConfigurationSets` does the same for configuration sets: it reports every
//!   stored set from its record, narrows by `ConfigurationSetNames` (name or ARN, each of
//!   which must name a stored set) and by `Filters`, and pages over the store's order;
//! * a send resolves every referenced resource against the store and mints a unique
//!   `MessageId` per accepted send.

use std::sync::Arc;

use service_com_amazonaws_pinpointsmsvoicev2_pinpointsmsvoicev2 as sms_voice;
use sms_voice::ops::types::{
    CONFIGURATION_SET_FILTER_NAME_VALUES, MESSAGE_TYPE_VALUES, NUMBER_CAPABILITY_VALUES,
    PHONE_NUMBER_FILTER_NAME_VALUES, REQUESTABLE_NUMBER_TYPE_VALUES,
};
use sms_voice::state::{MAX_CONFIGURATION_SETS_PER_ACCOUNT, MAX_RETAINED_MESSAGES};
use smithy_server_runtime::MultiServiceRouter;

/// The region these tests sign for, so the ARNs under assertion are not the default one.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries.
const ACCOUNT_ID: &str = emulator_server::EMULATOR_ACCOUNT_ID;

/// One emulator: the shared engine plus a router with only this service mounted.
///
/// The engine handle is returned alongside the router so a test can read the persisted
/// records directly — the same store the handlers write, reached the way they reach it.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests assert behaviour, not signatures; the emulator verifies signatures
        // itself and a service never checks them.
        router.set_request_authentication(false);
        router.register_service(sms_voice::EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Call one operation and answer `(http status, parsed JSON body)`.
    async fn call(&self, operation: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("PinpointSMSVoiceV2.{operation}"))
            // The SigV4 credential scope is what routes the request to this service and
            // what names the region its ARNs carry.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sms-voice/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
        };
        (status, parsed)
    }

    /// Call one operation and require HTTP 200, reporting the body when it is not.
    async fn ok(&self, operation: &str, body: serde_json::Value) -> serde_json::Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(status, 200, "{operation}: expected HTTP 200, got {status} with {parsed}");
        parsed
    }

    /// The messages the store holds, as `(message_id, destination, dry_run)`.
    fn stored_messages(&self) -> Vec<(String, String, bool)> {
        let store = sms_voice::state::service_state(&*self.engine).expect("this service's store");
        let state = store.lock();
        state
            .messages()
            .map(|message| {
                (
                    message.message_id.clone(),
                    message.destination_phone_number.clone(),
                    message.dry_run,
                )
            })
            .collect()
    }
}

/// The wire error code of a failed call (`__type`, with any namespace prefix dropped).
fn error_code(body: &serde_json::Value) -> String {
    let raw = body
        .get("__type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

fn as_str(body: &serde_json::Value, member: &str) -> String {
    body.get(member)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("expected a string member {member} in {body}"))
        .to_string()
}

/// Request one number with the given shape and answer its `PhoneNumberId`.
async fn request_number(emulator: &Emulator, body: serde_json::Value) -> String {
    let created = emulator.ok("RequestPhoneNumber", body).await;
    as_str(&created, "PhoneNumberId")
}

/// The baseline request body for a number: the smallest one the model accepts.
fn number_request(iso: &str, message_type: &str, capability: &str, number_type: &str) -> serde_json::Value {
    serde_json::json!({
        "IsoCountryCode": iso,
        "MessageType": message_type,
        "NumberCapabilities": [capability],
        "NumberType": number_type,
    })
}

#[tokio::test]
async fn the_configuration_set_and_phone_number_slice_round_trips_end_to_end() {
    let emulator = Emulator::new();

    // CREATE a configuration set; the response is read back out of the stored record.
    let created = emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({
                "ConfigurationSetName": "demo-set",
                "Tags": [{"Key": "env", "Value": "test"}],
            }),
        )
        .await;
    assert_eq!(
        as_str(&created, "ConfigurationSetName"),
        "demo-set",
        "CreateConfigurationSet: expected the created name, got {created}"
    );
    assert_eq!(
        as_str(&created, "ConfigurationSetArn"),
        format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:configuration-set/demo-set"),
        "CreateConfigurationSet: expected the region/account-scoped ARN, got {created}"
    );
    let created_at = created
        .get("CreatedTimestamp")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| panic!("CreateConfigurationSet: expected a CreatedTimestamp in {created}"));
    assert!(
        created_at > 1_700_000_000.0,
        "CreateConfigurationSet: expected an epoch-seconds CreatedTimestamp, got {created_at}"
    );
    assert_eq!(
        created.get("Tags"),
        Some(&serde_json::json!([{"Key": "env", "Value": "test"}])),
        "CreateConfigurationSet: expected the tags that were stored, got {created}"
    );

    // A name is unique per account: the second create conflicts.
    let (status, body) = emulator
        .call(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "demo-set"}),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ConflictException"),
        "a duplicate configuration-set name must conflict, got {status} with {body}"
    );

    // REQUEST a number; it is persisted, so describe reports it.
    let requested = emulator
        .ok(
            "RequestPhoneNumber",
            number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR"),
        )
        .await;
    let phone_number_id = as_str(&requested, "PhoneNumberId");
    let phone_number = as_str(&requested, "PhoneNumber");
    assert_eq!(
        as_str(&requested, "Status"),
        "ACTIVE",
        "RequestPhoneNumber: expected a provisioned number, got {requested}"
    );
    assert_eq!(
        as_str(&requested, "PhoneNumberArn"),
        format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:phone-number/{phone_number_id}"),
        "RequestPhoneNumber: expected the number's own ARN, got {requested}"
    );
    assert!(
        is_e164(&phone_number),
        "RequestPhoneNumber: expected an E.164 PhoneNumber, got {phone_number:?}"
    );
    assert_eq!(
        as_str(&requested, "OptOutListName"),
        "Default",
        "RequestPhoneNumber: a number requested with no opt-out list joins the default one, got {requested}"
    );
    for member in ["TwoWayEnabled", "SelfManagedOptOutsEnabled", "DeletionProtectionEnabled"] {
        assert_eq!(
            requested.get(member).and_then(serde_json::Value::as_bool),
            Some(false),
            "RequestPhoneNumber: {member} defaults to false, got {requested}"
        );
    }

    let described = emulator.ok("DescribePhoneNumbers", serde_json::json!({})).await;
    let listed = described
        .get("PhoneNumbers")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("DescribePhoneNumbers: expected PhoneNumbers in {described}"));
    assert_eq!(
        listed.len(),
        1,
        "DescribePhoneNumbers: expected the one requested number, got {described}"
    );
    assert_eq!(
        as_str(&listed[0], "PhoneNumberId"),
        phone_number_id,
        "DescribePhoneNumbers: expected the id that was minted, got {described}"
    );
    assert_eq!(
        as_str(&listed[0], "IsoCountryCode"),
        "US",
        "DescribePhoneNumbers: expected the stored country, got {described}"
    );
    assert_eq!(
        listed[0].get("NumberCapabilities"),
        Some(&serde_json::json!(["SMS"])),
        "DescribePhoneNumbers: expected the stored capabilities, got {described}"
    );

    // SEND: two accepted sends must carry two different message ids.
    let send = serde_json::json!({
        "DestinationPhoneNumber": "+14255550123",
        "OriginationIdentity": phone_number_id,
        "MessageBody": "hello",
        "ConfigurationSetName": "demo-set",
    });
    let first = as_str(&emulator.ok("SendTextMessage", send.clone()).await, "MessageId");
    let second = as_str(&emulator.ok("SendTextMessage", send.clone()).await, "MessageId");
    assert_ne!(
        first, second,
        "SendTextMessage: each accepted send mints its own MessageId, got {first} twice"
    );
    assert_eq!(
        emulator.stored_messages(),
        vec![
            (first.clone(), "+14255550123".to_string(), false),
            (second.clone(), "+14255550123".to_string(), false),
        ],
        "SendTextMessage: both sends must be recorded in order"
    );

    // An origination identity that names nothing is not found — not a 501.
    let (status, body) = emulator
        .call(
            "SendTextMessage",
            serde_json::json!({
                "DestinationPhoneNumber": "+14255550123",
                "OriginationIdentity": "pn-does-not-exist",
                "MessageBody": "hello",
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ResourceNotFoundException"),
        "an unknown origination identity must be not-found, got {status} with {body}"
    );

    // RELEASE: the response is the removed record, and the number is then absent.
    let released = emulator
        .ok(
            "ReleasePhoneNumber",
            serde_json::json!({"PhoneNumberId": phone_number_id}),
        )
        .await;
    assert_eq!(
        (
            as_str(&released, "PhoneNumberId"),
            as_str(&released, "PhoneNumber"),
            as_str(&released, "IsoCountryCode"),
            as_str(&released, "NumberType"),
        ),
        (
            phone_number_id.clone(),
            phone_number.clone(),
            "US".to_string(),
            "SIMULATOR".to_string(),
        ),
        "ReleasePhoneNumber: expected the released record's own fields, got {released}"
    );

    let after_release = emulator.ok("DescribePhoneNumbers", serde_json::json!({})).await;
    assert_eq!(
        after_release.get("PhoneNumbers"),
        Some(&serde_json::json!([])),
        "DescribePhoneNumbers: a released number must not be listed, got {after_release}"
    );
    let (status, body) = emulator.call("SendTextMessage", send).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ResourceNotFoundException"),
        "sending from a released number must be not-found, got {status} with {body}"
    );

    // DELETE the configuration set; the second delete is not found.
    let deleted = emulator
        .ok(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "demo-set"}),
        )
        .await;
    assert_eq!(
        as_str(&deleted, "ConfigurationSetArn"),
        format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:configuration-set/demo-set"),
        "DeleteConfigurationSet: expected the stored ARN, got {deleted}"
    );
    assert_eq!(
        deleted.get("CreatedTimestamp").and_then(serde_json::Value::as_f64),
        Some(created_at),
        "DeleteConfigurationSet: expected the CreatedTimestamp the set was created with, got {deleted}"
    );
    assert_eq!(
        deleted.get("EventDestinations"),
        Some(&serde_json::json!([])),
        "DeleteConfigurationSet: a set with no event destinations reports none, got {deleted}"
    );
    let (status, body) = emulator
        .call(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "demo-set"}),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ResourceNotFoundException"),
        "a second delete of the same set must be not-found, got {status} with {body}"
    );
}

/// `PhoneNumber` is `^\+?[1-9][0-9]{1,18}$` — E.164, optional leading `+`.
fn is_e164(value: &str) -> bool {
    let digits = value.strip_prefix('+').unwrap_or(value);
    (2..=19).contains(&digits.len())
        && digits.starts_with(|c: char| ('1'..='9').contains(&c))
        && digits.chars().all(|c| c.is_ascii_digit())
}

#[tokio::test]
async fn every_requestable_number_shape_is_reported_back_as_it_was_stored() {
    let emulator = Emulator::new();

    // The catalogs are the model's own enums, re-exported from `ops::types`:
    // `RequestableNumberType`, `MessageType` and `NumberCapability`.
    let mut cases: Vec<(String, serde_json::Value)> = Vec::new();
    for number_type in REQUESTABLE_NUMBER_TYPE_VALUES {
        for message_type in MESSAGE_TYPE_VALUES {
            for capability in NUMBER_CAPABILITY_VALUES {
                cases.push((
                    format!("{number_type}/{message_type}/{capability}"),
                    number_request("GB", message_type, capability, number_type),
                ));
            }
        }
    }
    // One row spanning the whole capability list (len 1..4) and the other country form,
    // so the list member is exercised at both of its published sizes.
    cases.push((
        "all-capabilities".to_string(),
        serde_json::json!({
            "IsoCountryCode": "CA",
            "MessageType": "PROMOTIONAL",
            "NumberCapabilities": NUMBER_CAPABILITY_VALUES,
            "NumberType": "TEN_DLC",
            "DeletionProtectionEnabled": true,
            "InternationalSendingEnabled": true,
            "OptOutListName": "my-list",
            "Tags": [{"Key": "team", "Value": "sms"}],
        }),
    ));

    let mut minted: Vec<(String, String, serde_json::Value)> = Vec::new();
    for (case, body) in &cases {
        let created = emulator.ok("RequestPhoneNumber", body.clone()).await;
        let id = as_str(&created, "PhoneNumberId");
        let number = as_str(&created, "PhoneNumber");
        assert!(
            is_e164(&number),
            "case {case:?}: expected an E.164 PhoneNumber, got {number:?}"
        );
        minted.push((case.clone(), id, body.clone()));
    }

    // Every minted number is distinct in both its id and its E.164 number.
    let mut ids: Vec<&String> = minted.iter().map(|(_, id, _)| id).collect();
    ids.sort();
    let distinct = ids.len();
    ids.dedup();
    assert_eq!(
        ids.len(),
        distinct,
        "every lease must mint its own PhoneNumberId, got {distinct} leases and {} distinct ids",
        ids.len()
    );

    // Read every one back through DescribePhoneNumbers and compare it with its request.
    for (case, id, body) in &minted {
        let described = emulator
            .ok(
                "DescribePhoneNumbers",
                serde_json::json!({"PhoneNumberIds": [id]}),
            )
            .await;
        let listed = &described
            .get("PhoneNumbers")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected PhoneNumbers in {described}"))[0];
        for member in ["IsoCountryCode", "MessageType", "NumberType"] {
            assert_eq!(
                listed.get(member),
                body.get(member),
                "case {case:?}: {member} must be reported as stored, got {listed}"
            );
        }
        assert_eq!(
            listed.get("NumberCapabilities"),
            body.get("NumberCapabilities"),
            "case {case:?}: NumberCapabilities must be reported as stored, got {listed}"
        );
        assert_eq!(
            listed.get("DeletionProtectionEnabled").and_then(serde_json::Value::as_bool),
            Some(
                body.get("DeletionProtectionEnabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            ),
            "case {case:?}: DeletionProtectionEnabled must be reported as stored, got {listed}"
        );
        assert_eq!(
            as_str(listed, "Status"),
            "ACTIVE",
            "case {case:?}: expected a provisioned number, got {listed}"
        );
    }
}

#[tokio::test]
async fn describe_phone_numbers_narrows_on_every_modelled_filter_name() {
    let emulator = Emulator::new();

    // Two numbers that differ in every filterable attribute this slice can set, so each
    // filter name has a row that matches exactly one of them.
    let wanted = request_number(
        &emulator,
        serde_json::json!({
            "IsoCountryCode": "US",
            "MessageType": "TRANSACTIONAL",
            "NumberCapabilities": ["SMS"],
            "NumberType": "SIMULATOR",
            "OptOutListName": "wanted-list",
            "DeletionProtectionEnabled": true,
        }),
    )
    .await;
    let other = request_number(
        &emulator,
        serde_json::json!({
            "IsoCountryCode": "GB",
            "MessageType": "PROMOTIONAL",
            "NumberCapabilities": ["VOICE"],
            "NumberType": "TOLL_FREE",
            "OptOutListName": "other-list",
            "DeletionProtectionEnabled": false,
        }),
    )
    .await;

    // One row per `PhoneNumberFilterName` the model declares: the value that selects
    // `wanted`, and how many of the two numbers it is expected to select.
    // `two-way-channel-arn` selects neither, because neither number has a two-way
    // channel and a record with no value for an attribute matches no filter value.
    let cases: &[(&str, &str, usize)] = &[
        ("status", "ACTIVE", 2),
        ("iso-country-code", "US", 1),
        ("message-type", "TRANSACTIONAL", 1),
        ("number-capability", "SMS", 1),
        ("number-type", "SIMULATOR", 1),
        ("two-way-enabled", "false", 2),
        ("self-managed-opt-outs-enabled", "false", 2),
        ("opt-out-list-name", "wanted-list", 1),
        ("deletion-protection-enabled", "true", 1),
        ("two-way-channel-arn", "arn:aws:sns:us-west-2:000000000000:topic/none", 0),
    ];
    assert_eq!(
        cases.len(),
        PHONE_NUMBER_FILTER_NAME_VALUES.len(),
        "every modelled PhoneNumberFilterName needs a row: the model declares {:?}",
        PHONE_NUMBER_FILTER_NAME_VALUES
    );

    for (name, value, expected) in cases {
        let described = emulator
            .ok(
                "DescribePhoneNumbers",
                serde_json::json!({"Filters": [{"Name": name, "Values": [value]}]}),
            )
            .await;
        let listed = described
            .get("PhoneNumbers")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("case {name:?}: expected PhoneNumbers in {described}"));
        assert_eq!(
            listed.len(),
            *expected,
            "case {name:?}={value:?}: expected {expected} match(es), got {described}"
        );
        if *expected == 1 {
            assert_eq!(
                as_str(&listed[0], "PhoneNumberId"),
                wanted,
                "case {name:?}={value:?}: expected the number that carries the value, got {described}"
            );
        }
    }

    // Values within one filter are OR-ed; separate filters are AND-ed.
    let both = emulator
        .ok(
            "DescribePhoneNumbers",
            serde_json::json!({"Filters": [{"Name": "iso-country-code", "Values": ["US", "GB"]}]}),
        )
        .await;
    assert_eq!(
        both.get("PhoneNumbers").and_then(serde_json::Value::as_array).map(Vec::len),
        Some(2),
        "case \"OR within one filter\": expected both numbers, got {both}"
    );
    let neither = emulator
        .ok(
            "DescribePhoneNumbers",
            serde_json::json!({"Filters": [
                {"Name": "iso-country-code", "Values": ["US"]},
                {"Name": "number-type", "Values": ["TOLL_FREE"]},
            ]}),
        )
        .await;
    assert_eq!(
        neither.get("PhoneNumbers"),
        Some(&serde_json::json!([])),
        "case \"AND across filters\": no number is both US and TOLL_FREE, got {neither}"
    );
    assert_ne!(wanted, other, "the two fixture numbers must be distinct");
}

#[tokio::test]
async fn describe_phone_numbers_pages_the_whole_set_at_every_page_size() {
    let emulator = Emulator::new();

    let mut all_ids = Vec::new();
    for index in 0..7 {
        all_ids.push(
            request_number(
                &emulator,
                number_request(
                    "US",
                    MESSAGE_TYPE_VALUES[index % MESSAGE_TYPE_VALUES.len()],
                    "SMS",
                    "SIMULATOR",
                ),
            )
            .await,
        );
    }
    all_ids.sort();

    // The whole set is reachable by paging, at every page size from the published
    // minimum to one past the number of records, with no id skipped or repeated.
    for page_size in [1usize, 2, 3, 6, 7, 8, 100] {
        let mut seen = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = serde_json::json!({"MaxResults": page_size});
            if let Some(token) = &next_token {
                request["NextToken"] = serde_json::Value::String(token.clone());
            }
            let page = emulator.ok("DescribePhoneNumbers", request).await;
            let listed = page
                .get("PhoneNumbers")
                .and_then(serde_json::Value::as_array)
                .unwrap_or_else(|| panic!("case page_size={page_size}: expected PhoneNumbers in {page}"));
            assert!(
                listed.len() <= page_size,
                "case page_size={page_size}: a page must not exceed MaxResults, got {} entries",
                listed.len()
            );
            for entry in listed {
                seen.push(as_str(entry, "PhoneNumberId"));
            }
            next_token = page
                .get("NextToken")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            pages += 1;
            assert!(
                pages <= 16,
                "case page_size={page_size}: paging did not terminate after {pages} pages"
            );
            if next_token.is_none() {
                break;
            }
        }
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(
            sorted, all_ids,
            "case page_size={page_size}: paging must yield every id exactly once, got {seen:?}"
        );
    }

    // `MaxResults` is modelled `range 1..100`: both published bounds are accepted and a
    // value past either end is refused before the handler runs.
    for accepted in [1, 100] {
        let (status, body) = emulator
            .call("DescribePhoneNumbers", serde_json::json!({"MaxResults": accepted}))
            .await;
        assert_eq!(
            status, 200,
            "case MaxResults={accepted}: a published bound must be accepted, got {status} with {body}"
        );
    }
    for refused in [0, 101] {
        let (status, body) = emulator
            .call("DescribePhoneNumbers", serde_json::json!({"MaxResults": refused}))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case MaxResults={refused}: a value past the modelled range must be refused, got {status} with {body}"
        );
    }

    // A cursor this service did not issue is refused, not silently treated as a page
    // boundary — including a bare offset, which carries no prefix this handler minted.
    for forged in ["not-a-cursor", "1", "00000002", "pnlist-", "PNLIST-00000001"] {
        let (status, body) = emulator
            .call(
                "DescribePhoneNumbers",
                serde_json::json!({"NextToken": forged}),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {forged:?}: a forged NextToken must be refused, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn an_origination_identity_resolves_in_every_form_that_names_the_number() {
    let emulator = Emulator::new();

    let configuration_set = "send-set";
    emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": configuration_set}),
        )
        .await;
    let created = emulator
        .ok(
            "RequestPhoneNumber",
            number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR"),
        )
        .await;
    let id = as_str(&created, "PhoneNumberId");
    let arn = as_str(&created, "PhoneNumberArn");
    let number = as_str(&created, "PhoneNumber");

    // `TextMessageOriginationIdentity` admits the id, the ARN and the phone number; the
    // number's `^\+?...` pattern makes the leading `+` optional, so both forms must work.
    let cases: Vec<(&str, String)> = vec![
        ("PhoneNumberId", id.clone()),
        ("PhoneNumberArn", arn.clone()),
        ("PhoneNumber", number.clone()),
        ("PhoneNumber without +", number.trim_start_matches('+').to_string()),
    ];
    let mut message_ids = Vec::new();
    for (case, identity) in &cases {
        let sent = emulator
            .ok(
                "SendTextMessage",
                serde_json::json!({
                    "DestinationPhoneNumber": "+14255550199",
                    "OriginationIdentity": identity,
                    "MessageBody": "hi",
                    // Named by ARN on one row, by name on the rest: the member is a
                    // `ConfigurationSetNameOrArn`.
                    "ConfigurationSetName": if *case == "PhoneNumberArn" {
                        format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:configuration-set/{configuration_set}")
                    } else {
                        configuration_set.to_string()
                    },
                }),
            )
            .await;
        message_ids.push(as_str(&sent, "MessageId"));
    }
    let mut distinct = message_ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        cases.len(),
        "each accepted send mints its own MessageId, got {message_ids:?}"
    );
    assert_eq!(
        emulator.stored_messages().len(),
        cases.len(),
        "every accepted send must be recorded, got {:?}",
        emulator.stored_messages()
    );

    // A configuration set that does not exist is not found, whichever way it is named.
    for case in ["no-such-set", "arn:aws:sms-voice:us-west-2:000000000000:configuration-set/no-such-set"] {
        let (status, body) = emulator
            .call(
                "SendTextMessage",
                serde_json::json!({
                    "DestinationPhoneNumber": "+14255550199",
                    "OriginationIdentity": id,
                    "MessageBody": "hi",
                    "ConfigurationSetName": case,
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: an unknown configuration set must be not-found, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn a_dry_run_mints_a_message_id_without_counting_as_a_sent_message() {
    let emulator = Emulator::new();
    let created = emulator
        .ok(
            "RequestPhoneNumber",
            number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR"),
        )
        .await;
    let id = as_str(&created, "PhoneNumberId");

    // DryRun is a modelled boolean with the default `false`; both values are rows here.
    for dry_run in [true, false] {
        let sent = emulator
            .ok(
                "SendTextMessage",
                serde_json::json!({
                    "DestinationPhoneNumber": "+14255550111",
                    "OriginationIdentity": id,
                    "MessageBody": "body",
                    "DryRun": dry_run,
                }),
            )
            .await;
        assert!(
            !as_str(&sent, "MessageId").is_empty(),
            "case DryRun={dry_run}: a MessageId is returned either way, got {sent}"
        );
    }

    let stored = emulator.stored_messages();
    assert_eq!(
        stored.iter().map(|(_, _, dry)| *dry).collect::<Vec<bool>>(),
        vec![true, false],
        "the dry run must be recorded as a dry run, not as a send: {stored:?}"
    );
    assert_eq!(
        stored.iter().filter(|(_, _, dry)| !*dry).count(),
        1,
        "exactly one of the two calls counts as a sent message: {stored:?}"
    );
}

#[tokio::test]
async fn a_repeated_client_token_creates_nothing_further() {
    let emulator = Emulator::new();

    // CreateConfigurationSet: the same token returns the set it already created, rather
    // than conflicting on the name or creating a second one.
    let first = emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "idem-set", "ClientToken": "token-1"}),
        )
        .await;
    let again = emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "idem-set", "ClientToken": "token-1"}),
        )
        .await;
    assert_eq!(
        (as_str(&again, "ConfigurationSetArn"), again.get("CreatedTimestamp")),
        (as_str(&first, "ConfigurationSetArn"), first.get("CreatedTimestamp")),
        "case \"repeated ClientToken\": expected the record the token already created, got {again}"
    );

    // A DIFFERENT token for the same name is a genuine duplicate and conflicts.
    let (status, body) = emulator
        .call(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "idem-set", "ClientToken": "token-2"}),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ConflictException"),
        "case \"new token, same name\": expected a conflict, got {status} with {body}"
    );

    // The SAME token with a different request is a different request: AWS replays a token
    // only for an identical one, so this is refused rather than answered with the set the
    // token created.
    for (case, request) in [
        (
            "same token, different name",
            serde_json::json!({"ConfigurationSetName": "other-set", "ClientToken": "token-1"}),
        ),
        (
            "same token, different tags",
            serde_json::json!({
                "ConfigurationSetName": "idem-set",
                "ClientToken": "token-1",
                "Tags": [{"Key": "env", "Value": "test"}],
            }),
        ),
    ] {
        let (status, response) = emulator.call("CreateConfigurationSet", request).await;
        assert_eq!(
            (
                status,
                error_code(&response).as_str(),
                response.get("Message").and_then(serde_json::Value::as_str),
            ),
            (
                400,
                "ConflictException",
                Some(sms_voice::state::CLIENT_TOKEN_CONFLICT_MESSAGE),
            ),
            "case {case:?}: a token replayed with a different request must be refused, got {status} with {response}"
        );
    }

    // RequestPhoneNumber: the same token leases no second number.
    let mut body = number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR");
    body["ClientToken"] = serde_json::Value::String("number-token".to_string());
    let first = emulator.ok("RequestPhoneNumber", body.clone()).await;
    let again = emulator.ok("RequestPhoneNumber", body.clone()).await;
    assert_eq!(
        as_str(&again, "PhoneNumberId"),
        as_str(&first, "PhoneNumberId"),
        "case \"repeated ClientToken\": expected the number the token already minted, got {again}"
    );
    let described = emulator.ok("DescribePhoneNumbers", serde_json::json!({})).await;
    assert_eq!(
        described.get("PhoneNumbers").and_then(serde_json::Value::as_array).map(Vec::len),
        Some(1),
        "case \"repeated ClientToken\": exactly one number must exist, got {described}"
    );

    // The same token asking for a different number is a different request.
    let mut different = body.clone();
    different["NumberType"] = serde_json::Value::String("TOLL_FREE".to_string());
    let (status, response) = emulator.call("RequestPhoneNumber", different).await;
    assert_eq!(
        (
            status,
            error_code(&response).as_str(),
            response.get("Message").and_then(serde_json::Value::as_str),
        ),
        (
            400,
            "ConflictException",
            Some(sms_voice::state::CLIENT_TOKEN_CONFLICT_MESSAGE),
        ),
        "case \"same token, different NumberType\": expected a refusal, got {status} with {response}"
    );
}

#[tokio::test]
async fn deletion_protection_refuses_the_release_and_keeps_the_number() {
    let emulator = Emulator::new();

    let cases: &[(bool, u16, &str)] = &[
        (false, 200, "a number without deletion protection is released"),
        (true, 400, "a number with deletion protection stays leased"),
    ];
    for (protected, expected_status, case) in cases {
        let mut body = number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR");
        body["DeletionProtectionEnabled"] = serde_json::Value::Bool(*protected);
        let id = request_number(&emulator, body).await;

        let (status, response) = emulator
            .call("ReleasePhoneNumber", serde_json::json!({"PhoneNumberId": id}))
            .await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with {response}"
        );
        if *expected_status == 400 {
            assert_eq!(
                error_code(&response),
                "ConflictException",
                "case {case:?}: expected a conflict, got {response}"
            );
        }

        let described = emulator
            .ok(
                "DescribePhoneNumbers",
                serde_json::json!({"Filters": [{"Name": "deletion-protection-enabled", "Values": [protected.to_string()]}]}),
            )
            .await;
        let still_there = described
            .get("PhoneNumbers")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            still_there,
            usize::from(*protected),
            "case {case:?}: expected the number to be {}, got {described}",
            if *protected { "kept" } else { "gone" }
        );
    }

    // An id that names nothing is not found.
    let (status, body) = emulator
        .call(
            "ReleasePhoneNumber",
            serde_json::json!({"PhoneNumberId": "phone-00000000000000000000000000000099"}),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ResourceNotFoundException"),
        "case \"unknown PhoneNumberId\": expected not-found, got {status} with {body}"
    );
}

#[tokio::test]
async fn describe_phone_numbers_refuses_an_id_that_names_nothing() {
    let emulator = Emulator::new();
    let known = request_number(
        &emulator,
        number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR"),
    )
    .await;

    // A selection is all-or-nothing: one unknown id fails the whole describe, whether it
    // is alone or beside a known one.
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("only unknown", serde_json::json!(["phone-does-not-exist"])),
        (
            "known beside unknown",
            serde_json::json!([known.clone(), "phone-does-not-exist"]),
        ),
    ];
    for (case, ids) in cases {
        let (status, body) = emulator
            .call(
                "DescribePhoneNumbers",
                serde_json::json!({"PhoneNumberIds": ids}),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: expected not-found, got {status} with {body}"
        );
    }

    // The known id alone still resolves, by id and by ARN.
    let by_arn = format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:phone-number/{known}");
    for (case, id) in [("by id", known.clone()), ("by ARN", by_arn)] {
        let described = emulator
            .ok("DescribePhoneNumbers", serde_json::json!({"PhoneNumberIds": [id]}))
            .await;
        assert_eq!(
            described
                .get("PhoneNumbers")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1),
            "case {case:?}: expected the one number, got {described}"
        );
    }
}

#[tokio::test]
async fn every_caller_driven_store_is_bounded() {
    let emulator = Emulator::new();

    // A resource store is bounded by its per-account quota: the create past the cap is
    // refused with the modelled quota error, so the store stops growing.
    for index in 0..MAX_CONFIGURATION_SETS_PER_ACCOUNT {
        emulator
            .ok(
                "CreateConfigurationSet",
                serde_json::json!({"ConfigurationSetName": format!("set-{index}")}),
            )
            .await;
    }
    let (status, body) = emulator
        .call(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "one-too-many"}),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ServiceQuotaExceededException"),
        "case \"one past the configuration-set quota\": expected the quota refusal, got {status} with {body}"
    );
    // Deleting one frees exactly one slot, so the bound is a cap and not a one-way door.
    emulator
        .ok(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "set-0"}),
        )
        .await;
    emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "one-too-many"}),
        )
        .await;

    // The message log is a ring: past its cap its size stops changing and the oldest
    // record is the one dropped.
    let id = request_number(
        &emulator,
        number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR"),
    )
    .await;
    let mut minted = Vec::new();
    for index in 0..(MAX_RETAINED_MESSAGES + 5) {
        let sent = emulator
            .ok(
                "SendTextMessage",
                serde_json::json!({
                    "DestinationPhoneNumber": "+14255550123",
                    "OriginationIdentity": id,
                    "MessageBody": format!("message {index}"),
                }),
            )
            .await;
        minted.push(as_str(&sent, "MessageId"));
    }
    let retained = emulator.stored_messages();
    assert_eq!(
        retained.len(),
        MAX_RETAINED_MESSAGES,
        "case \"5 sends past the message cap\": the log must stop growing at {MAX_RETAINED_MESSAGES}, got {}",
        retained.len()
    );
    let retained_ids: Vec<&String> = retained.iter().map(|(message_id, _, _)| message_id).collect();
    assert_eq!(
        retained_ids.first().map(|id| id.as_str()),
        Some(minted[5].as_str()),
        "case \"5 sends past the message cap\": the 5 oldest sends must be the ones dropped, got {:?}",
        retained_ids.first()
    );
    assert_eq!(
        retained_ids.last().map(|id| id.as_str()),
        minted.last().map(String::as_str),
        "case \"5 sends past the message cap\": the newest send must be retained, got {:?}",
        retained_ids.last()
    );
}

/// One `Timestamp` member of a response body, as the number the wire carried.
///
/// `pointer` is a JSON pointer, so a member nested in a list (`DescribePhoneNumbers`)
/// is reachable too.
fn timestamp_member(body: &serde_json::Value, pointer: &str) -> serde_json::Value {
    body.pointer(pointer)
        .cloned()
        .unwrap_or_else(|| panic!("expected a timestamp at {pointer} in {body}"))
}

/// An epoch-seconds instant below every plausible "now", so a stamped value can be told
/// apart from a zero/default one without asserting on the clock.
const EPOCH_SECONDS_FLOOR: u64 = 1_700_000_000;

#[tokio::test]
async fn every_timestamp_this_service_stamps_is_reported_in_whole_epoch_seconds() {
    let emulator = Emulator::new();

    // Every response member this service stamps with the wall clock, one row each: the
    // two configuration-set operations that report `CreatedTimestamp` and the three
    // phone-number ones, named with the JSON pointer the member sits at. Each row is
    // collected as `(case, member)` and asserted below, so the rule is checked over the
    // whole class rather than on one operation.
    let mut stamped: Vec<(&str, serde_json::Value)> = Vec::new();

    let created_set = emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "ts-probe"}),
        )
        .await;
    stamped.push((
        "CreateConfigurationSet.CreatedTimestamp",
        timestamp_member(&created_set, "/CreatedTimestamp"),
    ));

    let requested = emulator
        .ok(
            "RequestPhoneNumber",
            number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR"),
        )
        .await;
    stamped.push((
        "RequestPhoneNumber.CreatedTimestamp",
        timestamp_member(&requested, "/CreatedTimestamp"),
    ));
    let phone_number_id = as_str(&requested, "PhoneNumberId");

    let described = emulator.ok("DescribePhoneNumbers", serde_json::json!({})).await;
    stamped.push((
        "DescribePhoneNumbers.PhoneNumbers[0].CreatedTimestamp",
        timestamp_member(&described, "/PhoneNumbers/0/CreatedTimestamp"),
    ));

    let released = emulator
        .ok(
            "ReleasePhoneNumber",
            serde_json::json!({"PhoneNumberId": phone_number_id}),
        )
        .await;
    stamped.push((
        "ReleasePhoneNumber.CreatedTimestamp",
        timestamp_member(&released, "/CreatedTimestamp"),
    ));

    let deleted_set = emulator
        .ok(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "ts-probe"}),
        )
        .await;
    stamped.push((
        "DeleteConfigurationSet.CreatedTimestamp",
        timestamp_member(&deleted_set, "/CreatedTimestamp"),
    ));

    for (case, member) in &stamped {
        // `as_u64()` answers `None` for a number the wire carried with a fractional part
        // (`1794300627.7584016` parses as a float), which is exactly the form under test:
        // an epoch-seconds timestamp is stamped at whole-second granularity.
        let Some(seconds) = member.as_u64() else {
            panic!(
                "case {case:?}: expected a whole-second epoch timestamp, got {member} \
                 (a sub-second remainder of the host clock is never reported)"
            );
        };
        assert!(
            seconds > EPOCH_SECONDS_FLOOR,
            "case {case:?}: expected an epoch-seconds instant later than \
             {EPOCH_SECONDS_FLOOR}, got {seconds}"
        );
    }

    // The same record read twice reports the same instant: truncation happens on one
    // shared helper, so it is stable rather than re-derived per response.
    assert_eq!(
        timestamp_member(&requested, "/CreatedTimestamp"),
        timestamp_member(&described, "/PhoneNumbers/0/CreatedTimestamp"),
        "case \"one record read twice\": the request, describe and release of one number \
         must report one instant, got {requested} then {described}"
    );
    assert_eq!(
        timestamp_member(&requested, "/CreatedTimestamp"),
        timestamp_member(&released, "/CreatedTimestamp"),
        "case \"one record read twice\": the release must report the stamped instant, \
         got {released}"
    );
}

/// The three `Set*` calls that configure one configuration set's defaults, as
/// `(operation, request member, value)` rows for one case.
fn default_settings(
    message_type: &str,
    sender_id: &str,
    feedback_enabled: bool,
) -> Vec<(&'static str, &'static str, serde_json::Value)> {
    vec![
        (
            "SetDefaultMessageType",
            "MessageType",
            serde_json::Value::from(message_type),
        ),
        (
            "SetDefaultSenderId",
            "SenderId",
            serde_json::Value::from(sender_id),
        ),
        (
            "SetDefaultMessageFeedbackEnabled",
            "MessageFeedbackEnabled",
            serde_json::Value::from(feedback_enabled),
        ),
    ]
}

#[tokio::test]
async fn a_configuration_set_default_that_was_set_is_reported_on_every_later_read() {
    let emulator = Emulator::new();

    // One row per shape of default. `MessageType` walks the modelled enum
    // (`MESSAGE_TYPE_VALUES`, the model's `MessageType` values), and the sender ids span
    // `SenderId`'s own constraint (len 1..11, `^[A-Za-z0-9_-]+$`): one character, the
    // 11-character maximum, and the `_`/digit forms the pattern allows.
    let cases: Vec<(&str, &str, &str, bool, bool)> = vec![
        // (case, MessageType, SenderId, MessageFeedbackEnabled, name the set by ARN)
        ("shortest sender id, feedback on", MESSAGE_TYPE_VALUES[0], "A", true, false),
        (
            "longest sender id, feedback off",
            MESSAGE_TYPE_VALUES[1],
            "SENDER-1234",
            false,
            false,
        ),
        (
            "underscored sender id, named by ARN, feedback on",
            MESSAGE_TYPE_VALUES[1],
            "svc_01",
            true,
            true,
        ),
    ];

    for (index, (case, message_type, sender_id, feedback_enabled, by_arn)) in
        cases.iter().enumerate()
    {
        let name = format!("defaults-{index}");
        let created = emulator
            .ok(
                "CreateConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
        // A set reports no defaults until one is set: the create-time state, which is
        // what the later reads must be told apart from.
        assert_eq!(
            (
                created.get("DefaultMessageType"),
                created.get("DefaultSenderId"),
            ),
            (None, None),
            "case {case:?}: a new set carries no defaults, got {created}"
        );

        // `ConfigurationSetName` is a `ConfigurationSetNameOrArn`, so one row names the
        // set by its ARN instead.
        let named = if *by_arn {
            format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:configuration-set/{name}")
        } else {
            name.clone()
        };

        for (operation, member, value) in
            default_settings(message_type, sender_id, *feedback_enabled)
        {
            let response = emulator
                .ok(
                    operation,
                    serde_json::json!({"ConfigurationSetName": named, member: value}),
                )
                .await;
            // The set call answers the value the request named, under the resolved name
            // and the set's ARN.
            assert_eq!(
                (
                    response.get(member),
                    as_str(&response, "ConfigurationSetName"),
                    as_str(&response, "ConfigurationSetArn"),
                ),
                (
                    Some(&value),
                    name.clone(),
                    format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:configuration-set/{name}"),
                ),
                "case {case:?}: {operation} must report the {member} it was asked for, got {response}"
            );
        }

        // The round trip: the implemented reader reports all three defaults off the
        // record the `Set*` calls wrote.
        let deleted = emulator
            .ok(
                "DeleteConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
        assert_eq!(
            (
                deleted.get("DefaultMessageType").and_then(serde_json::Value::as_str),
                deleted.get("DefaultSenderId").and_then(serde_json::Value::as_str),
                deleted
                    .get("DefaultMessageFeedbackEnabled")
                    .and_then(serde_json::Value::as_bool),
            ),
            (
                // A read of the set reports the record's canonical capitalization of the
                // message type, whichever case the request that set it used; the sender
                // id is a caller-chosen value, reported exactly as it was set.
                Some(sms_voice::state::canonical_message_type(message_type).as_str()),
                Some(*sender_id),
                Some(*feedback_enabled),
            ),
            "case {case:?}: the set's configured defaults must be reported back, got {deleted}"
        );
    }
}

#[tokio::test]
async fn setting_a_default_again_replaces_it_and_an_unknown_set_is_not_found() {
    let emulator = Emulator::new();

    emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "rewritten"}),
        )
        .await;

    // Each default is single-valued: a second set replaces the first, including turning
    // the message-feedback flag back off.
    for (operation, member, value) in default_settings(MESSAGE_TYPE_VALUES[1], "FIRST", true) {
        emulator
            .ok(
                operation,
                serde_json::json!({"ConfigurationSetName": "rewritten", member: value}),
            )
            .await;
    }
    for (operation, member, value) in default_settings(MESSAGE_TYPE_VALUES[0], "SECOND", false) {
        emulator
            .ok(
                operation,
                serde_json::json!({"ConfigurationSetName": "rewritten", member: value}),
            )
            .await;
    }
    let deleted = emulator
        .ok(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "rewritten"}),
        )
        .await;
    assert_eq!(
        (
            deleted.get("DefaultMessageType").and_then(serde_json::Value::as_str),
            deleted.get("DefaultSenderId").and_then(serde_json::Value::as_str),
            deleted
                .get("DefaultMessageFeedbackEnabled")
                .and_then(serde_json::Value::as_bool),
        ),
        (
            Some(sms_voice::state::canonical_message_type(MESSAGE_TYPE_VALUES[0]).as_str()),
            Some("SECOND"),
            Some(false),
        ),
        "case \"each default set twice\": the last write must be the one reported, got {deleted}"
    );

    // A default can only be set on a set that exists: an unknown name, and the name of
    // the set just deleted, are both not-found.
    for (case, named) in [
        ("never created", "no-such-set"),
        ("deleted set", "rewritten"),
    ] {
        for (operation, member, value) in default_settings(MESSAGE_TYPE_VALUES[0], "NOBODY", true) {
            let (status, body) = emulator
                .call(
                    operation,
                    serde_json::json!({"ConfigurationSetName": named, member: value}),
                )
                .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ResourceNotFoundException"),
                "case {case:?}: {operation} on a set that does not exist must be \
                 not-found, got {status} with {body}"
            );
        }
    }

    // The message type a request may name is the modelled `MessageType` value set
    // (`MESSAGE_TYPE_VALUES`), so the record's capitalization is never one a caller can
    // ask for: anything outside the set is refused rather than stored.
    emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "strict"}),
        )
        .await;
    for spelling in ["Transactional", "transactional", "URGENT"] {
        let (status, body) = emulator
            .call(
                "SetDefaultMessageType",
                serde_json::json!({"ConfigurationSetName": "strict", "MessageType": spelling}),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {spelling:?}: a message type outside the modelled set must be refused, \
             got {status} with {body}"
        );
    }
    let still_unset = emulator
        .ok(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "strict"}),
        )
        .await;
    assert_eq!(
        still_unset.get("DefaultMessageType"),
        None,
        "case \"refused message types\": a refused set must store nothing, got {still_unset}"
    );
}

/// One conflict case: the call that provokes it, and the resource members the refusal
/// names (`None` for a conflict that names no resource).
struct ConflictCase {
    case: &'static str,
    operation: &'static str,
    request: serde_json::Value,
    members: Option<(&'static str, &'static str, String)>,
}

#[tokio::test]
async fn a_conflict_reports_the_message_that_restates_its_own_members() {
    let emulator = Emulator::new();

    emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "taken", "ClientToken": "first-token"}),
        )
        .await;
    let mut protected = number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR");
    protected["DeletionProtectionEnabled"] = serde_json::Value::Bool(true);
    let protected_id = request_number(&emulator, protected).await;

    // One row per conflict this slice can report, differing in reason and resource type:
    // the message restates the exception's own `Reason`, `ResourceType` and `ResourceId`.
    let cases: Vec<ConflictCase> = vec![
        ConflictCase {
            case: "a second configuration set of one name",
            operation: "CreateConfigurationSet",
            request: serde_json::json!({"ConfigurationSetName": "taken"}),
            members: Some((
                "RESOURCE_ALREADY_EXISTS",
                "configuration-set",
                "taken".to_string(),
            )),
        },
        ConflictCase {
            case: "releasing a deletion-protected number",
            operation: "ReleasePhoneNumber",
            request: serde_json::json!({"PhoneNumberId": protected_id.clone()}),
            members: Some((
                "DELETION_PROTECTION_ENABLED",
                "phone-number",
                protected_id.clone(),
            )),
        },
        ConflictCase {
            // A replayed token names no resource, so the conflict carries the message
            // alone and none of the resource members.
            case: "a ClientToken replayed with a different request",
            operation: "CreateConfigurationSet",
            request: serde_json::json!({
                "ConfigurationSetName": "other",
                "ClientToken": "first-token",
            }),
            members: None,
        },
    ];

    for ConflictCase {
        case,
        operation,
        request,
        members,
    } in cases
    {
        let (status, body) = emulator.call(operation, request).await;
        let expected_message = match &members {
            Some((reason, resource_type, resource_id)) => {
                sms_voice::state::conflict_message(reason, resource_type, resource_id)
            }
            None => sms_voice::state::CLIENT_TOKEN_CONFLICT_MESSAGE.to_string(),
        };
        assert_eq!(
            (
                status,
                error_code(&body).as_str(),
                body.get("Message").and_then(serde_json::Value::as_str),
            ),
            (400, "ConflictException", Some(expected_message.as_str())),
            "case {case:?}: expected the conflict AWS words for it, got {status} with {body}"
        );
        assert_eq!(
            (
                body.get("Reason").and_then(serde_json::Value::as_str),
                body.get("ResourceType").and_then(serde_json::Value::as_str),
                body.get("ResourceId").and_then(serde_json::Value::as_str),
            ),
            match &members {
                Some((reason, resource_type, resource_id)) =>
                    (Some(*reason), Some(*resource_type), Some(resource_id.as_str())),
                None => (None, None, None),
            },
            "case {case:?}: expected the structured members the message restates, got {body}"
        );
    }
}

/// One refusal case: the call that provokes it, the error code it must carry and the
/// `Message` AWS words for it.
///
/// `expected_message` is spelled out by the rows themselves rather than taken from the
/// handler's own helper, so a change to the wording this service renders fails here.
struct MessageCase {
    case: &'static str,
    operation: &'static str,
    request: serde_json::Value,
    code: &'static str,
    expected_message: String,
}

/// `ConflictException`'s message as this service renders it: the fixed prefix and the
/// three quoted clauses, filled from the exception's own members.
fn conflict_text(reason: &str, resource_type: &str, resource_id: &str) -> String {
    format!(
        "Conflict Occurred - Reason=\"{reason}\" ResourceType=\"{resource_type}\" \
         ResourceId=\"{resource_id}\""
    )
}

/// `ResourceNotFoundException`'s message as this service renders it: the same convention
/// over the two members that exception carries.
fn not_found_text(resource_type: &str, resource_id: &str) -> String {
    format!("Resource Not Found - ResourceType=\"{resource_type}\" ResourceId=\"{resource_id}\"")
}

/// The longest name `ConfigurationSetName` accepts (len 1..64, pattern `^[A-Za-z0-9_-]+$`,
/// per `smithy_query(query="operation",operation="CreateConfigurationSet")`), so one row
/// drives the template with an id at the modelled maximum.
fn longest_configuration_set_name() -> String {
    let name = "Zz_-".repeat(16);
    assert_eq!(
        name.len(),
        64,
        "case \"longest name\": the row must sit on the modelled maximum, got {name:?}"
    );
    name
}

#[tokio::test]
async fn every_conflict_and_not_found_message_restates_the_members_it_carries() {
    let emulator = Emulator::new();

    // Two sets whose names sit on the modelled bounds (1 and 64 characters), so the
    // rendered id is not one shape of name only.
    let longest = longest_configuration_set_name();
    for name in ["s", longest.as_str()] {
        emulator
            .ok(
                "CreateConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
    }
    let protected_id = {
        let mut request = number_request("GB", "PROMOTIONAL", "VOICE", "LONG_CODE");
        request["DeletionProtectionEnabled"] = serde_json::Value::Bool(true);
        request_number(&emulator, request).await
    };
    let leased_id = request_number(
        &emulator,
        number_request("US", "TRANSACTIONAL", "SMS", "SIMULATOR"),
    )
    .await;

    // One row per refusal this slice can report: both message templates, four of the
    // modelled `ResourceType` values and two of the modelled `ConflictExceptionReason`
    // values (`smithy_query(query="shape",shape="ResourceType,ConflictExceptionReason")`),
    // over ids of every form this service renders — a name at each modelled bound, a name
    // the caller gave as an ARN, and a generated id.
    let cases: Vec<MessageCase> = vec![
        MessageCase {
            case: "a second configuration set of a one-character name",
            operation: "CreateConfigurationSet",
            request: serde_json::json!({"ConfigurationSetName": "s"}),
            code: "ConflictException",
            expected_message: conflict_text(
                "RESOURCE_ALREADY_EXISTS",
                "configuration-set",
                "s",
            ),
        },
        MessageCase {
            case: "a second configuration set of the longest name",
            operation: "CreateConfigurationSet",
            request: serde_json::json!({"ConfigurationSetName": longest}),
            code: "ConflictException",
            expected_message: conflict_text(
                "RESOURCE_ALREADY_EXISTS",
                "configuration-set",
                &longest,
            ),
        },
        MessageCase {
            case: "releasing a deletion-protected number",
            operation: "ReleasePhoneNumber",
            request: serde_json::json!({"PhoneNumberId": protected_id.clone()}),
            code: "ConflictException",
            expected_message: conflict_text(
                "DELETION_PROTECTION_ENABLED",
                "phone-number",
                &protected_id,
            ),
        },
        MessageCase {
            case: "deleting a configuration set that was never created",
            operation: "DeleteConfigurationSet",
            request: serde_json::json!({"ConfigurationSetName": "absent-set"}),
            code: "ResourceNotFoundException",
            expected_message: not_found_text("configuration-set", "absent-set"),
        },
        MessageCase {
            case: "naming an absent configuration set by ARN",
            operation: "SetDefaultMessageType",
            request: serde_json::json!({
                "ConfigurationSetName": configuration_set_arn("absent-by-arn"),
                "MessageType": "TRANSACTIONAL",
            }),
            // The store is keyed by name, so the ARN resolves to the name it names and
            // that is the id the refusal reports.
            code: "ResourceNotFoundException",
            expected_message: not_found_text("configuration-set", "absent-by-arn"),
        },
        MessageCase {
            case: "releasing a number id that was never leased",
            operation: "ReleasePhoneNumber",
            request: serde_json::json!({"PhoneNumberId": "phone-number-0000"}),
            code: "ResourceNotFoundException",
            expected_message: not_found_text("phone-number", "phone-number-0000"),
        },
        MessageCase {
            case: "sending through an absent configuration set",
            operation: "SendTextMessage",
            request: serde_json::json!({
                "DestinationPhoneNumber": "+14255550123",
                "OriginationIdentity": leased_id.clone(),
                "MessageBody": "hello",
                "ConfigurationSetName": "absent-set",
            }),
            code: "ResourceNotFoundException",
            expected_message: not_found_text("configuration-set", "absent-set"),
        },
        MessageCase {
            case: "sending through an absent protect configuration",
            operation: "SendTextMessage",
            request: serde_json::json!({
                "DestinationPhoneNumber": "+14255550123",
                "OriginationIdentity": leased_id.clone(),
                "MessageBody": "hello",
                "ProtectConfigurationId": "protect-0000",
            }),
            code: "ResourceNotFoundException",
            expected_message: not_found_text("protect-configuration", "protect-0000"),
        },
        MessageCase {
            case: "sending from an origination identity that was never leased",
            operation: "SendTextMessage",
            request: serde_json::json!({
                "DestinationPhoneNumber": "+14255550123",
                "OriginationIdentity": "phone-number-0000",
                "MessageBody": "hello",
            }),
            code: "ResourceNotFoundException",
            expected_message: not_found_text("phone-number", "phone-number-0000"),
        },
    ];

    for MessageCase {
        case,
        operation,
        request,
        code,
        expected_message,
    } in cases
    {
        let (status, body) = emulator.call(operation, request).await;
        assert_eq!(
            (
                status,
                error_code(&body).as_str(),
                body.get("Message").and_then(serde_json::Value::as_str),
            ),
            (400, code, Some(expected_message.as_str())),
            "case {case:?}: expected the message AWS words for this refusal, \
             got {status} with {body}"
        );

        // The invariant behind the wording: the message restates the members the
        // exception itself carries, so a reader of either is told the same thing.
        let reason = body.get("Reason").and_then(serde_json::Value::as_str);
        let resource_type = body
            .get("ResourceType")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a ResourceType in {body}"));
        let resource_id = body
            .get("ResourceId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a ResourceId in {body}"));
        let rendered = match reason {
            Some(reason) => conflict_text(reason, resource_type, resource_id),
            None => not_found_text(resource_type, resource_id),
        };
        assert_eq!(
            body.get("Message").and_then(serde_json::Value::as_str),
            Some(rendered.as_str()),
            "case {case:?}: the message must restate this error's own members, got {body}"
        );
    }
}

/// The ARN this emulator stamps into a configuration set's record, for the region these
/// tests sign for.
fn configuration_set_arn(name: &str) -> String {
    format!("arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:configuration-set/{name}")
}

/// The `ConfigurationSetName` of every entry a `DescribeConfigurationSets` response
/// reports, in the order it reports them.
fn described_set_names(body: &serde_json::Value) -> Vec<String> {
    body.get("ConfigurationSets")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("expected a ConfigurationSets list in {body}"))
        .iter()
        .map(|entry| as_str(entry, "ConfigurationSetName"))
        .collect()
}

/// The sender id the rows below configure, so the value under assertion is a chosen one.
const DESCRIBED_SENDER_ID: &str = "SENDER-01";

#[tokio::test]
async fn describe_configuration_sets_reports_every_stored_set_from_its_record() {
    let emulator = Emulator::new();

    // An account with no configuration sets reports an empty list, not an absent member,
    // and a complete page carries no cursor.
    let empty = emulator
        .ok("DescribeConfigurationSets", serde_json::json!({}))
        .await;
    assert_eq!(
        (empty.get("ConfigurationSets"), empty.get("NextToken")),
        (Some(&serde_json::json!([])), None),
        "case \"no sets stored\": expected an empty page and no cursor, got {empty}"
    );

    // One row per shape of set: the name's form and length (`ConfigurationSetName`,
    // len 1..64, `^[A-Za-z0-9_-]+$`), whether the set carries tags, and whether its
    // defaults were set. The describe reports each record as it was stored, not one shape.
    let longest_name = "z".repeat(64);
    let cases: Vec<(&str, String, bool, bool)> = vec![
        // (case, ConfigurationSetName, tagged, defaults configured)
        ("shortest name", "a".to_string(), false, false),
        ("hyphens and digits", "set-2-demo".to_string(), true, false),
        ("underscored name", "set_three".to_string(), false, true),
        ("longest name", longest_name.clone(), true, true),
        ("digits only", "4242".to_string(), false, false),
    ];

    // The create response's instant per set, so the describe can be held to the stamp the
    // record carries rather than to a fresh clock read.
    let mut created_timestamps: Vec<(String, serde_json::Value)> = Vec::new();
    for (case, name, tagged, with_defaults) in &cases {
        let mut request = serde_json::json!({"ConfigurationSetName": name});
        if *tagged {
            request["Tags"] = serde_json::json!([{"Key": "team", "Value": case}]);
        }
        let created = emulator.ok("CreateConfigurationSet", request).await;
        created_timestamps.push((
            name.clone(),
            created.get("CreatedTimestamp").cloned().unwrap_or_else(|| {
                panic!("case {case:?}: expected a CreatedTimestamp in {created}")
            }),
        ));
        if *with_defaults {
            for (operation, member, value) in
                default_settings(MESSAGE_TYPE_VALUES[0], DESCRIBED_SENDER_ID, true)
            {
                emulator
                    .ok(
                        operation,
                        serde_json::json!({"ConfigurationSetName": name, member: value}),
                    )
                    .await;
            }
        }
    }

    // With nothing selected, every set in the account is reported, in the store's stable
    // key order.
    let described = emulator
        .ok("DescribeConfigurationSets", serde_json::json!({}))
        .await;
    let mut expected_names: Vec<String> =
        cases.iter().map(|(_, name, _, _)| name.clone()).collect();
    expected_names.sort();
    assert_eq!(
        described_set_names(&described),
        expected_names,
        "case \"the whole account\": expected every stored set in key order, got {described}"
    );

    let entries = described
        .get("ConfigurationSets")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("expected a ConfigurationSets list in {described}"))
        .clone();
    for (case, name, _, with_defaults) in &cases {
        let entry = entries
            .iter()
            .find(|entry| as_str(entry, "ConfigurationSetName") == *name)
            .unwrap_or_else(|| panic!("case {case:?}: expected {name} in {described}"));
        let stamped = created_timestamps
            .iter()
            .find(|(stored, _)| stored == name)
            .map(|(_, at)| at.clone())
            .expect("every created set recorded its instant");
        assert_eq!(
            (
                as_str(entry, "ConfigurationSetArn"),
                entry.get("CreatedTimestamp").cloned(),
                entry.get("EventDestinations").cloned(),
            ),
            (
                configuration_set_arn(name),
                Some(stamped),
                // No event destination can be created yet, so every stored set reports
                // the empty list its record holds.
                Some(serde_json::json!([])),
            ),
            "case {case:?}: expected the stored ARN, instant and event destinations, got {entry}"
        );
        let defaults = (
            entry
                .get("DefaultMessageType")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            entry
                .get("DefaultSenderId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            entry
                .get("DefaultMessageFeedbackEnabled")
                .and_then(serde_json::Value::as_bool),
        );
        let expected_defaults = if *with_defaults {
            (
                // A read of the set reports the record's canonical capitalization of the
                // message type, whichever case the request that set it used.
                Some(sms_voice::state::canonical_message_type(MESSAGE_TYPE_VALUES[0])),
                Some(DESCRIBED_SENDER_ID.to_string()),
                Some(true),
            )
        } else {
            (None, None, Some(false))
        };
        assert_eq!(
            defaults, expected_defaults,
            "case {case:?}: expected the defaults the record holds, got {entry}"
        );
        // `ConfigurationSetInformation` models no `Tags` member and nothing associates a
        // protect configuration with a set, so neither is reported whatever the create
        // request carried.
        assert_eq!(
            (entry.get("Tags"), entry.get("ProtectConfigurationId")),
            (None, None),
            "case {case:?}: expected no Tags and no ProtectConfigurationId, got {entry}"
        );
    }
}

#[tokio::test]
async fn describe_configuration_sets_selects_the_sets_the_request_names() {
    let emulator = Emulator::new();

    let names = ["alpha", "bravo", "charlie", "delta", "echo"];
    for name in names {
        emulator
            .ok(
                "CreateConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
    }

    // `ConfigurationSetNames` entries are `ConfigurationSetNameOrArn`, so the rows differ
    // in form (bare name, ARN, both), in count (one up to the modelled maximum of five)
    // and in order: a selection is reported in the order the request named it, not in the
    // store's.
    let cases: Vec<(&str, Vec<String>, Vec<&str>)> = vec![
        ("one bare name", vec![names[0].to_string()], vec![names[0]]),
        (
            "one ARN",
            vec![configuration_set_arn(names[1])],
            vec![names[1]],
        ),
        (
            "a name and an ARN, out of key order",
            vec![names[2].to_string(), configuration_set_arn(names[0])],
            vec![names[2], names[0]],
        ),
        (
            "the five-entry maximum, reversed",
            names.iter().rev().map(|name| name.to_string()).collect(),
            names.iter().rev().copied().collect(),
        ),
        // One entry is one reported set, so a set named twice is reported twice.
        (
            "the same set named twice, by name and by ARN",
            vec![names[3].to_string(), configuration_set_arn(names[3])],
            vec![names[3], names[3]],
        ),
    ];

    for (case, selection, expected) in &cases {
        let described = emulator
            .ok(
                "DescribeConfigurationSets",
                serde_json::json!({"ConfigurationSetNames": selection}),
            )
            .await;
        assert_eq!(
            described_set_names(&described),
            expected.iter().map(|name| (*name).to_string()).collect::<Vec<String>>(),
            "case {case:?}: expected exactly the sets the request named, got {described}"
        );
    }

    // A name that names no stored set is refused: the doc says a configuration set name
    // that isn't valid is an error, and the refusal restates its own members.
    emulator
        .ok(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": names[4]}),
        )
        .await;
    let gone = configuration_set_arn("never-created");
    let not_found: Vec<(&str, &str, &str)> = vec![
        ("a name that was never created", "no-such-set", "no-such-set"),
        // An ARN resolves to its last segment, so the refusal names the set, not the ARN.
        ("an ARN of a set that was never created", &gone, "never-created"),
        ("a set that was deleted", names[4], names[4]),
    ];
    for (case, named, resolved) in not_found {
        let (status, body) = emulator
            .call(
                "DescribeConfigurationSets",
                serde_json::json!({"ConfigurationSetNames": [named]}),
            )
            .await;
        assert_eq!(
            (
                status,
                error_code(&body).as_str(),
                body.get("Message").and_then(serde_json::Value::as_str),
                body.get("ResourceId").and_then(serde_json::Value::as_str),
            ),
            (
                400,
                "ResourceNotFoundException",
                Some(
                    sms_voice::state::resource_not_found_message("configuration-set", resolved)
                        .as_str()
                ),
                Some(resolved),
            ),
            "case {case:?}: expected the not-found AWS words for it, got {status} with {body}"
        );
    }

    // The deleted set is gone from the unselected list too: the describe reads live state.
    let remaining = emulator
        .ok("DescribeConfigurationSets", serde_json::json!({}))
        .await;
    assert_eq!(
        described_set_names(&remaining),
        names[..4].iter().map(|name| (*name).to_string()).collect::<Vec<String>>(),
        "case \"after a delete\": expected only the sets that remain, got {remaining}"
    );
}

/// One configuration set the filter cases run against.
struct FilterFixture {
    /// Its `ConfigurationSetName`.
    name: &'static str,
    /// The defaults configured on it, or `None` to leave the set with none at all — a
    /// record with no value for an attribute matches no value of that filter.
    defaults: Option<FilterDefaults>,
}

/// The three defaults one [`FilterFixture`] configures.
struct FilterDefaults {
    message_type: &'static str,
    sender_id: &'static str,
    feedback_enabled: bool,
}

#[tokio::test]
async fn describe_configuration_sets_narrows_on_every_modelled_filter_name() {
    let emulator = Emulator::new();

    // Three sets that differ in every filterable attribute this slice can set: one with
    // each default configured, one with the other values, and one with no defaults at all
    // (a record with no value for an attribute matches no value of that filter).
    let fixtures: Vec<FilterFixture> = vec![
        FilterFixture {
            name: "wanted",
            defaults: Some(FilterDefaults {
                message_type: MESSAGE_TYPE_VALUES[0],
                sender_id: "WANTED",
                feedback_enabled: true,
            }),
        },
        FilterFixture {
            name: "other",
            defaults: Some(FilterDefaults {
                message_type: MESSAGE_TYPE_VALUES[1],
                sender_id: "OTHER",
                feedback_enabled: false,
            }),
        },
        FilterFixture {
            name: "plain",
            defaults: None,
        },
    ];
    for FilterFixture { name, defaults } in &fixtures {
        emulator
            .ok(
                "CreateConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
        if let Some(FilterDefaults {
            message_type,
            sender_id,
            feedback_enabled,
        }) = defaults
        {
            for (operation, member, value) in
                default_settings(message_type, sender_id, *feedback_enabled)
            {
                emulator
                    .ok(
                        operation,
                        serde_json::json!({"ConfigurationSetName": name, member: value}),
                    )
                    .await;
            }
        }
    }

    // One row per `ConfigurationSetFilterName` the model declares: the value under test
    // and the sets it is expected to select, in key order. `event-destination-name`,
    // `matching-event-types` and `protect-configuration-id` select nothing, because no
    // stored set carries an event destination or a protect configuration, and a record
    // with no value for an attribute matches no filter value.
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "default-message-type",
            sms_voice::state::canonical_message_type(MESSAGE_TYPE_VALUES[0]),
            vec!["wanted"],
        ),
        ("default-sender-id", "OTHER".to_string(), vec!["other"]),
        (
            "default-message-feedback-enabled",
            "false".to_string(),
            vec!["other", "plain"],
        ),
        ("event-destination-name", "any-destination".to_string(), vec![]),
        ("matching-event-types", "TEXT_DELIVERED".to_string(), vec![]),
        ("protect-configuration-id", "protect-01".to_string(), vec![]),
        // One stored value, both of its spellings: the capitalization a read of the record
        // reports, and the spelling the model's `MessageType` enum declares, which is what
        // a client filters with.
        (
            "default-message-type",
            MESSAGE_TYPE_VALUES[1].to_string(),
            vec!["other"],
        ),
    ];
    assert_eq!(
        cases
            .iter()
            .map(|(name, _, _)| *name)
            .collect::<std::collections::BTreeSet<&str>>(),
        CONFIGURATION_SET_FILTER_NAME_VALUES
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<&str>>(),
        "every modelled ConfigurationSetFilterName needs a row, and no row may name a \
         filter the model does not declare"
    );

    for (name, value, expected) in &cases {
        let described = emulator
            .ok(
                "DescribeConfigurationSets",
                serde_json::json!({"Filters": [{"Name": name, "Values": [value]}]}),
            )
            .await;
        assert_eq!(
            described_set_names(&described),
            expected.iter().map(|set| (*set).to_string()).collect::<Vec<String>>(),
            "case {name:?}={value:?}: expected the sets that carry the value, got {described}"
        );
    }

    // Values within one filter are OR-ed; separate filters are AND-ed.
    let either = emulator
        .ok(
            "DescribeConfigurationSets",
            serde_json::json!({"Filters": [
                {"Name": "default-sender-id", "Values": ["WANTED", "OTHER"]},
            ]}),
        )
        .await;
    assert_eq!(
        described_set_names(&either),
        vec!["other".to_string(), "wanted".to_string()],
        "case \"OR within one filter\": expected both configured sets, got {either}"
    );
    let neither = emulator
        .ok(
            "DescribeConfigurationSets",
            serde_json::json!({"Filters": [
                {"Name": "default-sender-id", "Values": ["WANTED"]},
                {"Name": "default-message-feedback-enabled", "Values": ["false"]},
            ]}),
        )
        .await;
    assert_eq!(
        neither.get("ConfigurationSets"),
        Some(&serde_json::json!([])),
        "case \"AND across filters\": no set is both WANTED and feedback-off, got {neither}"
    );

    // A filter narrows the selection the names made, rather than replacing it.
    let selected_and_filtered = emulator
        .ok(
            "DescribeConfigurationSets",
            serde_json::json!({
                "ConfigurationSetNames": ["plain", "wanted"],
                "Filters": [{"Name": "default-message-feedback-enabled", "Values": ["true"]}],
            }),
        )
        .await;
    assert_eq!(
        described_set_names(&selected_and_filtered),
        vec!["wanted".to_string()],
        "case \"names and filters together\": expected the named set that also matches, \
         got {selected_and_filtered}"
    );
}

#[tokio::test]
async fn describe_configuration_sets_pages_the_whole_account_at_every_page_size() {
    let emulator = Emulator::new();

    let mut all_names: Vec<String> = Vec::new();
    for index in 0..7 {
        let name = format!("paged-{index}");
        emulator
            .ok(
                "CreateConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
        all_names.push(name);
    }
    all_names.sort();

    // The whole account is reachable by paging, at every page size from the published
    // minimum to past the number of records, with no set skipped or repeated.
    for page_size in [1usize, 2, 3, 6, 7, 8, 100] {
        let mut seen: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = serde_json::json!({"MaxResults": page_size});
            if let Some(token) = &next_token {
                request["NextToken"] = serde_json::Value::String(token.clone());
            }
            let page = emulator.ok("DescribeConfigurationSets", request).await;
            let listed = described_set_names(&page);
            assert!(
                listed.len() <= page_size,
                "case page_size={page_size}: a page must not exceed MaxResults, got {listed:?}"
            );
            seen.extend(listed);
            next_token = page
                .get("NextToken")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            pages += 1;
            assert!(
                pages <= 16,
                "case page_size={page_size}: paging did not terminate after {pages} pages"
            );
            if next_token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, all_names,
            "case page_size={page_size}: paging must yield every set exactly once in key \
             order, got {seen:?}"
        );
    }

    // `MaxResults` is modelled `range 1..100`: both published bounds are accepted and a
    // value past either end is refused before the handler runs.
    for accepted in [1, 100] {
        let (status, body) = emulator
            .call(
                "DescribeConfigurationSets",
                serde_json::json!({"MaxResults": accepted}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case MaxResults={accepted}: a published bound must be accepted, got {status} \
             with {body}"
        );
    }
    for refused in [0, 101] {
        let (status, body) = emulator
            .call(
                "DescribeConfigurationSets",
                serde_json::json!({"MaxResults": refused}),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case MaxResults={refused}: a value past the modelled range must be refused, \
             got {status} with {body}"
        );
    }

    // A cursor this service did not issue is refused, not silently treated as a page
    // boundary — including a bare offset, which carries no prefix this handler minted.
    for forged in ["not-a-cursor", "1", "00000002", "cslist-", "CSLIST-00000001"] {
        let (status, body) = emulator
            .call(
                "DescribeConfigurationSets",
                serde_json::json!({"NextToken": forged}),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {forged:?}: a forged NextToken must be refused, got {status} with {body}"
        );
    }

    // A cursor this service DID issue, replayed after the sets it pointed past were
    // deleted, points past the end of the result set and is refused too.
    let first_page = emulator
        .ok(
            "DescribeConfigurationSets",
            serde_json::json!({"MaxResults": 1}),
        )
        .await;
    let issued = first_page
        .get("NextToken")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("expected a NextToken on a short page, got {first_page}"))
        .to_string();
    for name in &all_names {
        emulator
            .ok(
                "DeleteConfigurationSet",
                serde_json::json!({"ConfigurationSetName": name}),
            )
            .await;
    }
    let (status, body) = emulator
        .call(
            "DescribeConfigurationSets",
            serde_json::json!({"NextToken": issued}),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "case \"a cursor past the end of the result set\": expected a refusal, got {status} \
         with {body}"
    );
}

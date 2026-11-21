// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the two rules the model states on the caller-supplied
//! external identifiers of this service.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `chime` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! 1. IDEMPOTENCY. `CreateAttendeeRequest$ExternalUserId` is documented as "An idempotency
//!    token. Links the attendee to an identity managed by a builder application"
//!    (`smithy_query(query="doc",shape="CreateAttendeeRequest",member="ExternalUserId")`),
//!    so an external identity has at most ONE attendee in a meeting: a repeated create for
//!    that identity answers with the attendee the first one minted — the same `AttendeeId`
//!    and the same `JoinToken` — and `ListAttendees` reports exactly one attendee for it.
//!    The same identity in ANOTHER meeting is another attendee, because the token is scoped
//!    to the meeting the attendee lives in.
//! 2. RESERVED PREFIX. The same member, and `CreateMeetingRequest$ExternalMeetingId`, both
//!    document "Values that begin with `aws:` are reserved. You can't configure a value
//!    that uses this prefix. Case insensitive.", so a value that begins with it — in any
//!    casing — is refused with the modelled `BadRequestException` (HTTP 400) naming the
//!    input member, and nothing is created.
//! 3. CHARACTER PATTERN. The sentence BEFORE that one, in the same paragraph, documents
//!    "Pattern: `[-_&@+=,(){}\[\]\/«».:|'"#a-zA-Z0-9À-ÿ\s]*`", so a value carrying any
//!    character that class does not list is refused the same way — the modelled
//!    `BadRequestException` (HTTP 400) naming the input member — and nothing is created
//!    for it, while a value made only of admitted characters is stored and read back
//!    unchanged. Neither shape carries a Smithy `@pattern` trait, so nothing enforces this
//!    before a handler and these cases exercise the handlers' own judgement over the wire.
//!
//! The tables span what the rules generalise over: the FORM of the identifier (both ends of
//! the modelled `@length(2..64)`, characters from the documented pattern, a value that only
//! contains the reserved prefix, the prefix in upper, capitalised and mixed case, one that
//! merely starts like it), which operation supplies it (`CreateAttendee`,
//! `BatchCreateAttendee`, `CreateMeeting`, `CreateMeetingWithAttendees`), how many times it
//! is repeated, and which meeting it is repeated in. For rule 3 they span WHICH excluded
//! character the value carries (ASCII punctuation the class omits, and the code points
//! immediately outside the `À-ÿ` range it lists), WHERE in the value it sits (first, inside,
//! last), which member carries it, and which operation supplies that member — the two batch
//! paths refusing the ITEM as they refuse any other bad item.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names the `chime` signing name
/// this service publishes (`OPERATIONS.md`, `## Wire protocol`). The scope is how the
/// router picks the claiming service; the signature itself is never checked here, since
/// the harness turns request authentication off.
const SIGNED_FOR_CHIME: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTCHIME/20260101/us-east-1/chime/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The prefix the model reserves in both external identifiers.
const RESERVED_PREFIX: &str = "aws:";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    fn text(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }

    /// The `Attendee` of a `CreateAttendee` answer.
    fn attendee(&self) -> &Value {
        self.body
            .get("Attendee")
            .unwrap_or_else(|| panic!("response has no `Attendee` member: {self:?}"))
    }

    /// A list member of the body (`Attendees`, `Errors`), empty when absent.
    fn list(&self, name: &str) -> Vec<Value> {
        self.body
            .get(name)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
}

/// The emulator under test: ONE shared engine with this service mounted on it, reached the
/// way the production router reaches it.
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://meetings-chime.us-east-1.amazonaws.com{path}"))
            .header("authorization", SIGNED_FOR_CHIME)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_code = response
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
        // restJson1 carries the wire error code in `x-amzn-errortype`; fall back to the
        // body's `__type` so a missing header never reads as "no error code".
        let error_code = header_code.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
        });
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn create_meeting(&self, token: &str, external_meeting_id: &str) -> Reply {
        self.call(
            "POST",
            "/meetings",
            Some(&json!({
                "ClientRequestToken": token,
                "MediaRegion": "us-east-1",
                "ExternalMeetingId": external_meeting_id,
            })),
        )
        .await
    }

    /// Create a meeting and answer its minted `MeetingId`.
    async fn meeting_id(&self, token: &str) -> String {
        let reply = self.create_meeting(token, &format!("ext-{token}")).await;
        assert_eq!(
            reply.status, 200,
            "CreateMeeting {token:?}: expected 200, got {} with {:?}",
            reply.status, reply.body
        );
        reply.body["Meeting"]["MeetingId"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateMeeting {token:?} returned no MeetingId"))
            .to_string()
    }

    async fn create_attendee(&self, meeting_id: &str, external_user_id: &str) -> Reply {
        self.call(
            "POST",
            &format!("/meetings/{meeting_id}/attendees"),
            Some(&json!({ "ExternalUserId": external_user_id })),
        )
        .await
    }

    async fn batch_create_attendee(&self, meeting_id: &str, external_user_ids: &[&str]) -> Reply {
        let items: Vec<Value> = external_user_ids
            .iter()
            .map(|user| json!({ "ExternalUserId": user }))
            .collect();
        self.call(
            "POST",
            &format!("/meetings/{meeting_id}/attendees?operation=batch-create"),
            Some(&json!({ "Attendees": items })),
        )
        .await
    }

    async fn create_meeting_with_attendees(
        &self,
        token: &str,
        external_meeting_id: &str,
        external_user_ids: &[&str],
    ) -> Reply {
        let items: Vec<Value> = external_user_ids
            .iter()
            .map(|user| json!({ "ExternalUserId": user }))
            .collect();
        self.call(
            "POST",
            "/meetings?operation=create-attendees",
            Some(&json!({
                "ClientRequestToken": token,
                "MediaRegion": "us-east-1",
                "ExternalMeetingId": external_meeting_id,
                "Attendees": items,
            })),
        )
        .await
    }

    /// The attendees of `meeting_id`, as `ListAttendees` reports them.
    async fn list_attendees(&self, meeting_id: &str) -> Vec<Value> {
        let reply = self
            .call("GET", &format!("/meetings/{meeting_id}/attendees"), None)
            .await;
        assert_eq!(
            reply.status, 200,
            "ListAttendees({meeting_id}): expected 200, got {} with {:?}",
            reply.status, reply.body
        );
        reply.list("Attendees")
    }
}

/// The `(AttendeeId, JoinToken, ExternalUserId)` of an `Attendee` record.
fn identity_of(attendee: &Value) -> (String, String, String) {
    let field = |name: &str| {
        attendee
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("member `{name}` is not a string in {attendee:?}"))
            .to_string()
    };
    (
        field("AttendeeId"),
        field("JoinToken"),
        field("ExternalUserId"),
    )
}

/// `ExternalUserId`s spanning the member's own constraints
/// (`smithy_query(query="shape",shape="ExternalUserId")`: `@length(2..64)`, over the
/// documented pattern `[-_&@+=,(){}\[\]\/«».:|'"#a-zA-Z0-9À-ÿ\s]*`).
fn external_user_id_forms() -> Vec<(&'static str, String)> {
    vec![
        ("a plain identity", "user-plain".to_string()),
        ("the modelled minimum length of 2", "ab".to_string()),
        ("the modelled maximum length of 64", "u".repeat(64)),
        (
            "characters from the documented pattern",
            "a.b:c/d_e+f@g|h#i'j".to_string(),
        ),
        (
            "a value that merely starts like the reserved prefix",
            "awsome:user".to_string(),
        ),
    ]
}

/// RULE 1: `ExternalUserId` is an idempotency token, so a repeat answers with the attendee
/// the first create minted and the meeting holds exactly one attendee for that identity.
#[tokio::test]
async fn repeating_a_create_for_one_external_identity_answers_the_same_attendee() {
    let emulator = Emulator::new();

    for (index, (case, external_user_id)) in external_user_id_forms().into_iter().enumerate() {
        let meeting_id = emulator.meeting_id(&format!("tok-idem-{index}")).await;

        let first = emulator.create_attendee(&meeting_id, &external_user_id).await;
        assert_eq!(
            (first.status, first.error_code.as_deref()),
            (200, None),
            "case {case:?}: the first CreateAttendee must answer 200, got {} {:?} with {:?}",
            first.status,
            first.error_code,
            first.body
        );
        let created = identity_of(first.attendee());

        // Repeated twice, so the rule is pinned over more than one retry.
        for attempt in 1..=2u8 {
            let repeat = emulator.create_attendee(&meeting_id, &external_user_id).await;
            assert_eq!(
                (repeat.status, repeat.error_code.as_deref()),
                (200, None),
                "case {case:?} (repeat {attempt}): expected 200, got {} {:?} with {:?}",
                repeat.status,
                repeat.error_code,
                repeat.body
            );
            let answered = identity_of(repeat.attendee());
            assert_eq!(
                answered, created,
                "case {case:?} (repeat {attempt}): expected the attendee the first create \
                 minted {created:?}, got {answered:?}"
            );
        }

        // The batch create resolves the same token the same way, so a mixed retry does not
        // duplicate the identity either; and an item naming a FRESH identity is minted.
        // The fresh value stays inside the modelled maximum length of 64.
        let fresh = format!("{}-2", &external_user_id[..external_user_id.len().min(60)]);
        let batch = emulator
            .batch_create_attendee(&meeting_id, &[&external_user_id, &fresh, &fresh])
            .await;
        assert_eq!(
            (batch.status, batch.error_code.as_deref()),
            (200, None),
            "case {case:?}: BatchCreateAttendee expected 200, got {} {:?} with {:?}",
            batch.status,
            batch.error_code,
            batch.body
        );
        let returned: Vec<(String, String, String)> =
            batch.list("Attendees").iter().map(identity_of).collect();
        assert_eq!(
            returned.len(),
            3,
            "case {case:?}: expected one record per batch item, got {returned:?}"
        );
        assert_eq!(
            returned[0], created,
            "case {case:?}: expected the batch item for the known identity to answer the \
             stored attendee {created:?}, got {:?}",
            returned[0]
        );
        assert_eq!(
            returned[1], returned[2],
            "case {case:?}: expected one attendee for an identity named twice in one batch, \
             got {:?} and {:?}",
            returned[1], returned[2]
        );

        // The meeting holds exactly one attendee per identity.
        let listed = emulator.list_attendees(&meeting_id).await;
        let for_identity: Vec<&Value> = listed
            .iter()
            .filter(|attendee| attendee["ExternalUserId"].as_str() == Some(&external_user_id))
            .collect();
        assert_eq!(
            for_identity.len(),
            1,
            "case {case:?}: expected exactly one attendee for the identity, got {for_identity:?}"
        );
        assert_eq!(
            listed.len(),
            2,
            "case {case:?}: expected the meeting to hold the two identities asked for, got \
             {listed:?}"
        );

        // The token is scoped to the MEETING: the same identity in another meeting is
        // another attendee.
        let other_meeting = emulator.meeting_id(&format!("tok-idem-other-{index}")).await;
        let elsewhere = emulator
            .create_attendee(&other_meeting, &external_user_id)
            .await;
        let (other_id, other_token, _) = identity_of(elsewhere.attendee());
        assert!(
            other_id != created.0 && other_token != created.1,
            "case {case:?}: the same identity in another meeting must be another attendee, \
             got AttendeeId {other_id} / JoinToken {other_token} against {created:?}"
        );
    }
}

/// RULE 1, through `CreateMeetingWithAttendees`: an identity named twice in the request
/// that CREATES the meeting gets one attendee, reported for both items.
#[tokio::test]
async fn one_identity_named_twice_in_a_create_with_attendees_gets_one_attendee() {
    let emulator = Emulator::new();

    // Rows: how often the identity repeats and where in the list it sits.
    let rows: Vec<(&str, Vec<&str>, usize)> = vec![
        ("the same identity twice", vec!["user-a", "user-a"], 1),
        (
            "the same identity three times among others",
            vec!["user-a", "user-b", "user-a", "user-a"],
            2,
        ),
        (
            "distinct identities only",
            vec!["user-a", "user-b", "user-c"],
            3,
        ),
    ];

    for (index, (case, items, distinct)) in rows.into_iter().enumerate() {
        let reply = emulator
            .create_meeting_with_attendees(
                &format!("tok-cwa-{index}"),
                &format!("ext-cwa-{index}"),
                &items,
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {case:?}: expected 200, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let returned: Vec<(String, String, String)> =
            reply.list("Attendees").iter().map(identity_of).collect();
        assert_eq!(
            returned.len(),
            items.len(),
            "case {case:?}: expected one record per request item, got {returned:?}"
        );
        // Every item naming the same identity answers the same attendee.
        for (position, user) in items.iter().enumerate() {
            let first_position = items
                .iter()
                .position(|candidate| candidate == user)
                .expect("the item is in the list");
            assert_eq!(
                returned[position], returned[first_position],
                "case {case:?}: item {position} ({user}) must answer the attendee item \
                 {first_position} did, got {:?} against {:?}",
                returned[position], returned[first_position]
            );
        }

        let meeting_id = reply.body["Meeting"]["MeetingId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no MeetingId in {:?}", reply.body))
            .to_string();
        let listed = emulator.list_attendees(&meeting_id).await;
        assert_eq!(
            listed.len(),
            distinct,
            "case {case:?}: expected one attendee per distinct identity, got {listed:?}"
        );
    }
}

/// How a request naming the reserved prefix in `member` is refused, as this service words
/// it: fixed text apart from the input member it names.
fn reserved_prefix_message(member: &str) -> String {
    format!("Validation error for {member}, prefix '{RESERVED_PREFIX}' is reserved.")
}

/// RULE 2: a value beginning with the reserved prefix is refused with the modelled 400
/// naming the member, and nothing is created.
#[tokio::test]
async fn an_external_identifier_in_the_reserved_namespace_is_refused() {
    let emulator = Emulator::new();
    let meeting_id = emulator.meeting_id("tok-reserved").await;

    // Rows span the forms a reserved value takes: the bare prefix (the modelled minimum
    // length allows it), a short one, one at the modelled maximum length, the prefix with
    // pattern characters after it, and the UPPER- and mixed-case spellings of the prefix,
    // which the documentation reserves too ("Case insensitive.").
    let reserved: Vec<(&str, String)> = vec![
        ("the bare reserved prefix", RESERVED_PREFIX.to_string()),
        ("a short reserved value", format!("{RESERVED_PREFIX}x")),
        (
            "a reserved value at the modelled maximum length",
            format!("{RESERVED_PREFIX}{}", "u".repeat(60)),
        ),
        (
            "a reserved value with pattern characters",
            format!("{RESERVED_PREFIX}reserved/user_1"),
        ),
        ("the prefix in upper case", "AWS:".to_string()),
        ("the prefix in upper case with a value", "AWS:user".to_string()),
        ("the prefix capitalised", "Aws:user".to_string()),
        ("the prefix in mixed case", "aWs:user".to_string()),
    ];

    for (index, (case, value)) in reserved.into_iter().enumerate() {
        // `CreateAttendee`: the whole call is refused and the meeting gains no attendee.
        let attendee = emulator.create_attendee(&meeting_id, &value).await;
        assert_eq!(
            (
                attendee.status,
                attendee.error_code.as_deref(),
                attendee.text("Message")
            ),
            (
                400,
                Some("BadRequestException"),
                Some(reserved_prefix_message("ExternalUserId").as_str())
            ),
            "case {case:?} (CreateAttendee): expected the modelled 400 naming the member, \
             got {} {:?} with {:?}",
            attendee.status,
            attendee.error_code,
            attendee.body
        );
        assert!(
            emulator.list_attendees(&meeting_id).await.is_empty(),
            "case {case:?} (CreateAttendee): a refused create must mint nothing"
        );

        // `BatchCreateAttendee`: the ITEM is refused, reported in `Errors` naming the value
        // the caller sent, while the item beside it is created.
        let batch = emulator
            .batch_create_attendee(&meeting_id, &[&value, "user-admitted"])
            .await;
        assert_eq!(
            (batch.status, batch.error_code.as_deref()),
            (200, None),
            "case {case:?} (BatchCreateAttendee): a per-item refusal must not fail the call, \
             got {} {:?} with {:?}",
            batch.status,
            batch.error_code,
            batch.body
        );
        let errors = batch.list("Errors");
        assert_eq!(
            errors.len(),
            1,
            "case {case:?} (BatchCreateAttendee): expected exactly the reserved item to be \
             refused, got {errors:?}"
        );
        assert_eq!(
            (
                errors[0]["ExternalUserId"].as_str(),
                errors[0]["ErrorCode"].as_str(),
                errors[0]["ErrorMessage"].as_str()
            ),
            (
                Some(value.as_str()),
                Some("BadRequest"),
                Some(reserved_prefix_message("ExternalUserId").as_str())
            ),
            "case {case:?} (BatchCreateAttendee): expected the item refusal to name the \
             value sent and the rule, got {errors:?}"
        );
        let admitted = emulator.list_attendees(&meeting_id).await;
        assert_eq!(
            admitted.len(),
            1,
            "case {case:?} (BatchCreateAttendee): expected only the admitted item stored, \
             got {admitted:?}"
        );
        // Leave the meeting as the next row found it.
        let attendee_id = admitted[0]["AttendeeId"]
            .as_str()
            .expect("a stored attendee carries its id");
        let dropped = emulator
            .call(
                "DELETE",
                &format!("/meetings/{meeting_id}/attendees/{attendee_id}"),
                None,
            )
            .await;
        assert_eq!(
            dropped.status, 204,
            "case {case:?}: expected DeleteAttendee to answer 204, got {} with {:?}",
            dropped.status, dropped.body
        );

        // The SAME rule on the meeting's own external id, in both creates: refused, and no
        // meeting is created for it.
        let meeting = emulator
            .create_meeting(&format!("tok-reserved-meeting-{index}"), &value)
            .await;
        assert_eq!(
            (
                meeting.status,
                meeting.error_code.as_deref(),
                meeting.text("Message")
            ),
            (
                400,
                Some("BadRequestException"),
                Some(reserved_prefix_message("ExternalMeetingId").as_str())
            ),
            "case {case:?} (CreateMeeting): expected the modelled 400 naming the member, got \
             {} {:?} with {:?}",
            meeting.status,
            meeting.error_code,
            meeting.body
        );
        let with_attendees = emulator
            .create_meeting_with_attendees(
                &format!("tok-reserved-cwa-{index}"),
                &value,
                &["user-with-reserved-meeting"],
            )
            .await;
        assert_eq!(
            (
                with_attendees.status,
                with_attendees.error_code.as_deref(),
                with_attendees.text("Message")
            ),
            (
                400,
                Some("BadRequestException"),
                Some(reserved_prefix_message("ExternalMeetingId").as_str())
            ),
            "case {case:?} (CreateMeetingWithAttendees): expected the modelled 400 naming \
             the member, got {} {:?} with {:?}",
            with_attendees.status,
            with_attendees.error_code,
            with_attendees.body
        );
        // The refused create is not an idempotency record either: its token still mints a
        // meeting when the request is corrected.
        let corrected = emulator
            .create_meeting(
                &format!("tok-reserved-meeting-{index}"),
                &format!("ext-corrected-{index}"),
            )
            .await;
        assert_eq!(
            (corrected.status, corrected.error_code.as_deref()),
            (200, None),
            "case {case:?}: expected the corrected create to be served, got {} {:?} with {:?}",
            corrected.status,
            corrected.error_code,
            corrected.body
        );
    }

    // A value that does not BEGIN with the prefix is admitted, however much of it it
    // carries: the rule is about the prefix, not about the characters.
    let admitted: Vec<(&str, &str)> = vec![
        ("the prefix without its colon", "aws"),
        ("the prefix as a suffix", "user:aws:"),
        ("the prefix after another character", " aws:user"),
        ("a value the prefix is a prefix of a word of", "awsx:user"),
    ];
    for (index, (case, value)) in admitted.into_iter().enumerate() {
        let meeting = emulator
            .create_meeting(&format!("tok-admitted-{index}"), value)
            .await;
        assert_eq!(
            (meeting.status, meeting.error_code.as_deref()),
            (200, None),
            "case {case:?} (CreateMeeting): expected it admitted, got {} {:?} with {:?}",
            meeting.status,
            meeting.error_code,
            meeting.body
        );
        let attendee = emulator.create_attendee(&meeting_id, value).await;
        assert_eq!(
            (attendee.status, attendee.error_code.as_deref()),
            (200, None),
            "case {case:?} (CreateAttendee): expected it admitted, got {} {:?} with {:?}",
            attendee.status,
            attendee.error_code,
            attendee.body
        );
    }
}

/// The character class the model documents on both identifiers, copied verbatim from
/// `Attendee$ExternalUserId` / `CreateMeetingRequest$ExternalMeetingId`
/// (`smithy_query(query="doc",shape="CreateAttendeeRequest",member="ExternalUserId")` ->
/// "Pattern: `[-_&@+=,(){}\[\]\/«».:|'\"#a-zA-Z0-9À-ÿ\s]*`").
const DOCUMENTED_PATTERN: &str = r##"[-_&@+=,(){}\[\]\/«».:|'"#a-zA-Z0-9À-ÿ\s]*"##;

/// How a request carrying a character the documented pattern excludes in `member` is
/// refused, as this service words it: fixed text apart from the input member it names and
/// the documented pattern it quotes. The VALUE is not in it — both members are
/// `@sensitive`.
fn pattern_message(member: &str) -> String {
    format!("Validation error for {member}, value must match the pattern '{DOCUMENTED_PATTERN}'.")
}

/// Characters the documented class does NOT list, so a value carrying one is outside the
/// pattern. The ASCII rows are punctuation absent from the class (which lists only
/// `-_&@+=,(){}[]/.:|'"#`); the last three are a code point far beyond the `À-ÿ` range the
/// class names and the two immediately outside its bounds.
const EXCLUDED_CHARACTERS: &[(&str, char)] = &[
    ("a dollar sign", '$'),
    ("a semicolon", ';'),
    ("an asterisk", '*'),
    ("a backslash", '\\'),
    ("a percent sign", '%'),
    ("a question mark", '?'),
    ("an angle bracket", '<'),
    ("an exclamation mark", '!'),
    ("a tilde", '~'),
    ("a caret", '^'),
    ("a backtick", '`'),
    ("a code point far beyond the À-ÿ range", '€'),
    ("the code point just below the À-ÿ range", '¿'),
    ("the code point just above the À-ÿ range", 'Ā'),
];

/// Values spanning the excluded characters and the POSITION one takes in the value. Every
/// value stays inside the member's modelled `@length(2..64)`, so what refuses it is the
/// pattern and not its length.
fn out_of_pattern_values() -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for (name, character) in EXCLUDED_CHARACTERS {
        for (position, value) in [
            ("first", format!("{character}user1")),
            ("inside", format!("user{character}1")),
            ("last", format!("user1{character}")),
        ] {
            rows.push((format!("{name} {position}"), value));
        }
    }
    rows
}

/// Values made only of characters the documented class lists: each group of it, both ends
/// of its `À-ÿ` range and a code point inside that range, plus `\s` whitespace.
fn in_pattern_values() -> Vec<(&'static str, String)> {
    vec![
        (
            "every punctuation character the class lists",
            r##"-_&@+=,(){}[]/.:|'"#"##.to_string(),
        ),
        ("the guillemets the class lists", "«quoted»".to_string()),
        ("ASCII letters and digits", "User0123".to_string()),
        ("whitespace", "two words".to_string()),
        (
            "the lower bound of the À-ÿ range",
            "\u{c0}-at-the-start".to_string(),
        ),
        (
            "the upper bound of the À-ÿ range",
            "\u{ff}-at-the-start".to_string(),
        ),
        ("a letter inside the À-ÿ range", "rené-é".to_string()),
    ]
}

/// RULE 3, through `CreateAttendee`: a value carrying a character the documented pattern
/// excludes is refused with the modelled 400 naming the member, and the meeting gains no
/// attendee for it.
#[tokio::test]
async fn an_external_user_id_outside_the_documented_pattern_is_refused() {
    let emulator = Emulator::new();
    let meeting_id = emulator.meeting_id("tok-pattern-attendee").await;
    let expected = pattern_message("ExternalUserId");

    for (case, value) in out_of_pattern_values() {
        let reply = emulator.create_attendee(&meeting_id, &value).await;
        assert_eq!(
            (
                reply.status,
                reply.error_code.as_deref(),
                reply.text("Message")
            ),
            (
                400,
                Some("BadRequestException"),
                Some(expected.as_str())
            ),
            "case {case:?} (CreateAttendee {value:?}): expected the modelled 400 naming the \
             member, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        assert!(
            emulator.list_attendees(&meeting_id).await.is_empty(),
            "case {case:?} ({value:?}): a refused create must mint nothing"
        );
    }
}

/// RULE 3 as a PER-ITEM refusal of `BatchCreateAttendee`: the offending item is reported in
/// `Errors` naming the value the caller sent, the item beside it is created, and the call
/// itself succeeds.
#[tokio::test]
async fn a_batch_item_outside_the_documented_pattern_is_refused_on_its_own() {
    let emulator = Emulator::new();
    let meeting_id = emulator.meeting_id("tok-pattern-batch").await;
    let expected = pattern_message("ExternalUserId");

    for (index, (case, value)) in out_of_pattern_values().into_iter().enumerate() {
        let admitted = format!("user-admitted-{index}");
        let reply = emulator
            .batch_create_attendee(&meeting_id, &[&value, &admitted])
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {case:?} ({value:?}): a per-item refusal must not fail the call, got {} \
             {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let errors = reply.list("Errors");
        assert_eq!(
            errors.len(),
            1,
            "case {case:?} ({value:?}): expected exactly the out-of-pattern item refused, \
             got {errors:?}"
        );
        assert_eq!(
            (
                errors[0]["ExternalUserId"].as_str(),
                errors[0]["ErrorCode"].as_str(),
                errors[0]["ErrorMessage"].as_str()
            ),
            (
                Some(value.as_str()),
                Some("BadRequest"),
                Some(expected.as_str())
            ),
            "case {case:?} ({value:?}): expected the item refusal to name the value sent and \
             the rule, got {errors:?}"
        );
        let stored = emulator.list_attendees(&meeting_id).await;
        let out_of_pattern: Vec<&Value> = stored
            .iter()
            .filter(|attendee| attendee["ExternalUserId"].as_str() == Some(value.as_str()))
            .collect();
        assert!(
            out_of_pattern.is_empty(),
            "case {case:?} ({value:?}): a refused item must mint nothing, got \
             {out_of_pattern:?}"
        );
        assert_eq!(
            stored.len(),
            index + 1,
            "case {case:?} ({value:?}): expected only the admitted item of each round stored, \
             got {stored:?}"
        );
    }
}

/// RULE 3 on the MEETING's own external id, through both creation paths: the whole call is
/// refused, no meeting is created, and the refused `ClientRequestToken` is free to serve
/// the corrected request.
#[tokio::test]
async fn an_external_meeting_id_outside_the_documented_pattern_is_refused() {
    let emulator = Emulator::new();
    let expected = pattern_message("ExternalMeetingId");

    for (index, (case, value)) in out_of_pattern_values().into_iter().enumerate() {
        let token = format!("tok-pattern-meeting-{index}");
        let reply = emulator.create_meeting(&token, &value).await;
        assert_eq!(
            (
                reply.status,
                reply.error_code.as_deref(),
                reply.text("Message")
            ),
            (
                400,
                Some("BadRequestException"),
                Some(expected.as_str())
            ),
            "case {case:?} (CreateMeeting {value:?}): expected the modelled 400 naming the \
             member, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );

        let with_attendees = emulator
            .create_meeting_with_attendees(
                &format!("tok-pattern-cwa-{index}"),
                &value,
                &["user-with-bad-meeting"],
            )
            .await;
        assert_eq!(
            (
                with_attendees.status,
                with_attendees.error_code.as_deref(),
                with_attendees.text("Message")
            ),
            (
                400,
                Some("BadRequestException"),
                Some(expected.as_str())
            ),
            "case {case:?} (CreateMeetingWithAttendees {value:?}): expected the modelled 400 \
             naming the member, got {} {:?} with {:?}",
            with_attendees.status,
            with_attendees.error_code,
            with_attendees.body
        );

        // The refused create left no idempotency record: the same token still mints a
        // meeting once the identifier is corrected.
        let corrected = emulator
            .create_meeting(&token, &format!("ext-corrected-{index}"))
            .await;
        assert_eq!(
            (corrected.status, corrected.error_code.as_deref()),
            (200, None),
            "case {case:?}: expected the corrected create to be served, got {} {:?} with {:?}",
            corrected.status,
            corrected.error_code,
            corrected.body
        );
    }
}

/// RULE 3 as a PER-ITEM refusal of `CreateMeetingWithAttendees`: the meeting is created,
/// the out-of-pattern item is reported in `Errors`, and only the admitted item becomes an
/// attendee of it.
#[tokio::test]
async fn a_create_with_attendees_item_outside_the_documented_pattern_is_refused_on_its_own() {
    let emulator = Emulator::new();
    let expected = pattern_message("ExternalUserId");

    for (index, (case, value)) in out_of_pattern_values().into_iter().enumerate() {
        let admitted = format!("user-admitted-{index}");
        let reply = emulator
            .create_meeting_with_attendees(
                &format!("tok-pattern-cwa-item-{index}"),
                &format!("ext-cwa-item-{index}"),
                &[&value, &admitted],
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {case:?} ({value:?}): a per-item refusal must not fail the call, got {} \
             {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let errors = reply.list("Errors");
        assert_eq!(
            errors.len(),
            1,
            "case {case:?} ({value:?}): expected exactly the out-of-pattern item refused, \
             got {errors:?}"
        );
        assert_eq!(
            (
                errors[0]["ExternalUserId"].as_str(),
                errors[0]["ErrorCode"].as_str(),
                errors[0]["ErrorMessage"].as_str()
            ),
            (
                Some(value.as_str()),
                Some("BadRequest"),
                Some(expected.as_str())
            ),
            "case {case:?} ({value:?}): expected the item refusal to name the value sent and \
             the rule, got {errors:?}"
        );
        let meeting_id = reply.body["Meeting"]["MeetingId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no MeetingId in {:?}", reply.body))
            .to_string();
        let stored = emulator.list_attendees(&meeting_id).await;
        assert_eq!(
            stored.len(),
            1,
            "case {case:?} ({value:?}): expected only the admitted item stored, got {stored:?}"
        );
        assert_eq!(
            stored[0]["ExternalUserId"].as_str(),
            Some(admitted.as_str()),
            "case {case:?} ({value:?}): expected the admitted identity stored, got {stored:?}"
        );
    }
}

/// The other side of RULE 3: a value made only of characters the documented class lists is
/// admitted by every creating operation and read back unchanged through `GetMeeting` and
/// `GetAttendee`.
#[tokio::test]
async fn an_external_identifier_inside_the_documented_pattern_round_trips() {
    let emulator = Emulator::new();

    for (index, (case, value)) in in_pattern_values().into_iter().enumerate() {
        // The meeting's own identifier, read back from the stored meeting.
        let meeting = emulator
            .create_meeting(&format!("tok-ok-{index}"), &value)
            .await;
        assert_eq!(
            (meeting.status, meeting.error_code.as_deref()),
            (200, None),
            "case {case:?} (CreateMeeting {value:?}): expected it admitted, got {} {:?} with \
             {:?}",
            meeting.status,
            meeting.error_code,
            meeting.body
        );
        let meeting_id = meeting.body["Meeting"]["MeetingId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no MeetingId in {:?}", meeting.body))
            .to_string();
        let read_meeting = emulator
            .call("GET", &format!("/meetings/{meeting_id}"), None)
            .await;
        assert_eq!(
            read_meeting.body["Meeting"]["ExternalMeetingId"].as_str(),
            Some(value.as_str()),
            "case {case:?} (GetMeeting): expected the stored identifier, got {:?}",
            read_meeting.body
        );

        // The attendee's identifier, read back from the stored attendee.
        let attendee = emulator.create_attendee(&meeting_id, &value).await;
        assert_eq!(
            (attendee.status, attendee.error_code.as_deref()),
            (200, None),
            "case {case:?} (CreateAttendee {value:?}): expected it admitted, got {} {:?} with \
             {:?}",
            attendee.status,
            attendee.error_code,
            attendee.body
        );
        let attendee_id = attendee.body["Attendee"]["AttendeeId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no AttendeeId in {:?}", attendee.body))
            .to_string();
        let read_attendee = emulator
            .call(
                "GET",
                &format!("/meetings/{meeting_id}/attendees/{attendee_id}"),
                None,
            )
            .await;
        assert_eq!(
            read_attendee.body["Attendee"]["ExternalUserId"].as_str(),
            Some(value.as_str()),
            "case {case:?} (GetAttendee): expected the stored identifier, got {:?}",
            read_attendee.body
        );

        // The two batch paths admit it too, refusing nothing.
        let batch = emulator
            .batch_create_attendee(&meeting_id, &[&value])
            .await;
        assert_eq!(
            (batch.status, batch.list("Errors").len()),
            (200, 0),
            "case {case:?} (BatchCreateAttendee {value:?}): expected it admitted and nothing \
             refused, got {} with {:?}",
            batch.status,
            batch.body
        );
        let with_attendees = emulator
            .create_meeting_with_attendees(&format!("tok-ok-cwa-{index}"), &value, &[&value])
            .await;
        assert_eq!(
            (with_attendees.status, with_attendees.list("Errors").len()),
            (200, 0),
            "case {case:?} (CreateMeetingWithAttendees {value:?}): expected it admitted and \
             nothing refused, got {} with {:?}",
            with_attendees.status,
            with_attendees.body
        );
    }
}

/// A value that breaks BOTH sentences of the documented paragraph — the reserved prefix AND
/// the pattern — is still the one modelled refusal naming the member, so a caller sees a
/// 400 whichever of the two rules the service reads first.
#[tokio::test]
async fn a_value_breaking_both_documented_rules_is_refused_once() {
    let emulator = Emulator::new();
    let meeting_id = emulator.meeting_id("tok-pattern-both").await;

    for (case, value) in [
        (
            "the reserved prefix with an excluded character",
            "aws:user$1",
        ),
        ("an excluded character before the prefix", "$aws:user"),
        (
            "the prefix in mixed case with an excluded character",
            "AwS:u*1",
        ),
    ] {
        let reply = emulator.create_attendee(&meeting_id, value).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (400, Some("BadRequestException")),
            "case {case:?} ({value:?}): expected the modelled 400, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let message = reply.text("Message").unwrap_or_default().to_string();
        assert!(
            message.contains("ExternalUserId"),
            "case {case:?} ({value:?}): expected the refusal to name the input member, got \
             {message:?}"
        );
        assert!(
            emulator.list_attendees(&meeting_id).await.is_empty(),
            "case {case:?} ({value:?}): a refused create must mint nothing"
        );
    }
}

// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for `CreatePresignedMlflowTrackingServerUrl`.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, on the
//! shared `Emulator` harness in `inference_deployment_lifecycle`; the published URL is
//! fetched the way a browser fetches it (`Emulator::fetch`, a plain `GET` of the path and
//! query the URL carries).
//!
//! The suites assert RULES over TABLES of inputs rather than one recorded call: the URL's
//! form for every region and every server shape, the fresh token each call mints, the
//! grant the call records on the STORED server, the window each of the two validity
//! members opens and closes, the `ResourceNotFound` refusal for a name no server was
//! created under, the two validity members at and past each published bound, and that
//! presigning changes nothing a later `DescribeMlflowTrackingServer` reports.
//!
//! The published URL addresses the MLflow session on THIS emulator's own endpoint, because
//! no client of the emulator resolves AWS's managed-UI host; the address form and the token
//! reader are the crate's shared ones, from `tests/all/presigned_session.rs`, so every
//! operation that publishes such a URL pins one address form.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state as sm;

use crate::inference_deployment_lifecycle::{
    error_code, message, str_field, Emulator, ACCOUNT,
};
// The crate's shared presigned-session helpers: the one address form every published URL
// takes and the one reader of the token it carries.
use crate::presigned_session::{expected_prefix as session_prefix, token_of};

/// The ARN resource kind a tracking server's session address names it by, as
/// `TrackingServerArn`'s modelled pattern spells it
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:mlflow-tracking-server/`).
const KIND: &str = "mlflow-tracking-server";

/// A loose floor on the authorization token's length: the emulator's credential store
/// mints a session token of several hundred characters, and this suite pins only that the
/// URL carries a substantial opaque token rather than the store's exact length.
const MIN_TOKEN_LENGTH: usize = 40;

/// A role ARN inside `RoleArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`).
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/mlflow-artifacts")
}

/// The smallest `CreateMlflowTrackingServer` request: the three members the model marks
/// required.
fn minimal(name: &str) -> Value {
    json!({
        "TrackingServerName": name,
        "ArtifactStoreUri": "s3://artifacts-bucket/mlflow",
        "RoleArn": role_arn(),
    })
}

/// Create a tracking server in `region` and answer the ARN the operation minted.
async fn create_in(emulator: &Emulator, case: &str, region: &str, body: &Value) -> String {
    let (status, created) = emulator.call_in(region, "CreateMlflowTrackingServer", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMlflowTrackingServer must answer HTTP 200, got {status} with \
         {created:?}"
    );
    str_field(&created, "TrackingServerArn")
}

/// The address the session of the tracking server called `name` is published at, in the
/// crate's one address form.
fn expected_prefix(name: &str) -> String {
    session_prefix(KIND, name)
}

/// The `authToken` query value of a presigned MLflow URL whose host and path are the ones
/// the rule prescribes for the server called `name`. Panics — with the case named — when
/// the URL takes any other form, so every caller gets the same structural check.
fn auth_token(case: &str, url: &str, name: &str) -> String {
    let prefix = expected_prefix(name);
    let token = url.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("case {case:?}: expected a URL starting {prefix:?}, got {url:?}")
    });

    // `TrackingServerUrl` is modelled len 0..2048.
    assert!(
        url.len() <= 2048,
        "case {case:?}: TrackingServerUrl is modelled len 0..2048, got {} bytes in {url:?}",
        url.len()
    );
    assert!(
        !url.contains("sagemaker.aws") && !url.contains("amazonaws.com"),
        "case {case:?}: a published URL must not name a host AWS serves, got {url:?}"
    );
    // One query parameter, carrying the authorization token: nothing else is appended.
    assert_eq!(
        url.matches('?').count(),
        1,
        "case {case:?}: expected exactly one query string, got {url:?}"
    );
    assert!(
        !token.contains('&'),
        "case {case:?}: expected the authToken parameter alone, got {url:?}"
    );
    // The token is percent-encoded, so the query value carries only RFC 3986 unreserved
    // characters and `%` escapes — never the `+` or `/` the minted alphabet includes.
    assert!(
        token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"%-._~".contains(&byte)),
        "case {case:?}: expected a percent-encoded token, got {token:?}"
    );
    assert!(
        token.len() >= MIN_TOKEN_LENGTH,
        "case {case:?}: expected a token of at least {MIN_TOKEN_LENGTH} characters, got {}",
        token.len()
    );
    token.to_string()
}

/// The grants the stored tracking server holds, exactly as the handler recorded them in
/// the one shared store every handler reaches.
///
/// The cross-subject suite's reader is keyed by its own `Subject` enum over the notebook,
/// partner-app and MLflow-App maps; this one reads the tracking-server map.
fn grants(emulator: &Emulator, name: &str) -> Vec<sm::PresignedUrlGrant> {
    let state =
        sm::service_state(&*emulator.engine).expect("the SageMaker store is on the shared engine");
    let guard = state.lock();
    guard
        .mlflow_tracking_servers
        .get(name)
        .map(|server| server.grants.clone())
        .unwrap_or_default()
}

/// Move every grant of the stored server `shift` into the past, so the window the mint
/// recorded has elapsed without the test waiting for it. The clock is what closes a
/// window, so a test that drives it this way needs no sleep and no wall-clock assertion.
///
/// The cross-subject suite's version is keyed by its own `Subject` enum, as `grants` is.
fn backdate_grants(emulator: &Emulator, name: &str, shift: Duration) {
    let state =
        sm::service_state(&*emulator.engine).expect("the SageMaker store is on the shared engine");
    let mut guard = state.lock();
    let server = guard
        .mlflow_tracking_servers
        .get_mut(name)
        .expect("the created tracking server is in the store");
    for grant in &mut server.grants {
        grant.issued_at -= shift;
        grant.expires_at -= shift;
        grant.session_expires_at -= shift;
    }
}

/// Move the instant the clock owes the stored server its `Creating` -> `Created`
/// transition into the past, so the transition is already due and the next reporting call
/// materialises it. The clock is what owes the transition, so a test that drives it this
/// way waits on nothing and asserts on no elapsed time.
fn due_now(emulator: &Emulator, name: &str) {
    let state =
        sm::service_state(&*emulator.engine).expect("the SageMaker store is on the shared engine");
    let mut guard = state.lock();
    let server = guard
        .mlflow_tracking_servers
        .get_mut(name)
        .expect("the created tracking server is in the store");
    server.created_due_at -= sm::TRACKING_SERVER_CREATED_AFTER;
}

/// RULE: the operation answers a presigned URL for the tracking server the store holds
/// under the requested name — the session address on this emulator's own endpoint, naming
/// the stored server and carrying a freshly minted authorization token recorded as a grant
/// on that server — whatever the server's members and whatever region it was created in.
#[tokio::test]
async fn a_presigned_url_names_the_stored_server_and_a_minted_authorization_token() {
    // Rows differ along the axes the rule generalises over: the signed region, the name's
    // length and shape (`TrackingServerName` is len 1..256, pattern
    // `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,255}$`), which optional create members the server
    // carries, and which of the two validity members the presign request names.
    let at_the_name_upper_bound = format!("a{}", "b".repeat(255));
    let cases: Vec<(&str, &str, Value, Value)> = vec![
        (
            "minimal-server-no-validity-members",
            "us-east-1",
            minimal("presign-minimal"),
            json!({}),
        ),
        (
            "name-at-the-lower-bound-with-expiry",
            "eu-west-3",
            minimal("z"),
            json!({ "ExpiresInSeconds": 60 }),
        ),
        (
            "name-at-the-upper-bound-with-session-window",
            "ap-southeast-2",
            minimal(&at_the_name_upper_bound),
            json!({ "SessionExpirationDurationInSeconds": 3_600 }),
        ),
        (
            "every-optional-create-member-and-both-validity-members",
            "us-gov-west-1",
            json!({
                "TrackingServerName": "presign-full",
                "ArtifactStoreUri": "s3://mlflow-artifacts/full/",
                "RoleArn": role_arn(),
                "TrackingServerSize": "Large",
                "MlflowVersion": "2.16.2",
                "AutomaticModelRegistration": true,
                "WeeklyMaintenanceWindowStart": "Tue:03:30",
                "Tags": [{ "Key": "team", "Value": "ml" }],
                "S3BucketOwnerAccountId": ACCOUNT,
                "S3BucketOwnerVerification": false,
            }),
            json!({ "ExpiresInSeconds": 300, "SessionExpirationDurationInSeconds": 43_200 }),
        ),
    ];

    let mut tokens: Vec<String> = Vec::new();
    for (case, region, create_body, validity) in &cases {
        let emulator = Emulator::new();
        let arn = create_in(&emulator, case, region, create_body).await;
        let name = str_field(create_body, "TrackingServerName");

        let mut body = validity.clone();
        body["TrackingServerName"] = json!(name.clone());
        let (status, parsed) = emulator
            .call_in(region, "CreatePresignedMlflowTrackingServerUrl", &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {parsed:?}"
        );
        assert_eq!(
            parsed.as_object().map(|members| members.len()),
            Some(1),
            "case {case:?}: the answer carries AuthorizedUrl alone, got {parsed:?}"
        );

        let url = str_field(&parsed, "AuthorizedUrl");
        let token = auth_token(case, &url, &name);
        assert!(
            !tokens.contains(&token),
            "case {case:?}: every call must mint a distinct token, got one twice"
        );
        tokens.push(token);

        // The token is a GRANT the server holds, not a value invented for the response.
        let decoded = token_of(&url);
        let held = grants(&emulator, &name);
        let grant = held
            .iter()
            .find(|grant| grant.token == decoded)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the token in {url:?} must be a grant the server holds, got \
                     {held:?}"
                )
            });
        // The windows the request asked for are the windows the grant carries: the URL's
        // own validity and the session it opens, each read off the request or the
        // emulator's documented default (`ExpiresInSeconds` 300, session 1800 seconds).
        let expires_in = validity
            .get("ExpiresInSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(300);
        let session_seconds = validity
            .get("SessionExpirationDurationInSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(1_800);
        assert_eq!(
            grant.expires_at,
            grant.issued_at + Duration::from_secs(expires_in),
            "case {case:?}: the URL window must be the one asked for ({expires_in}s), got \
             {grant:?}"
        );
        assert_eq!(
            grant.session_expires_at,
            grant.issued_at + Duration::from_secs(session_seconds),
            "case {case:?}: the session window must be the one asked for ({session_seconds}s), \
             got {grant:?}"
        );

        // Fetching the published URL opens the session that grant describes, on this
        // emulator, and names the server the ARN the create minted belongs to.
        let (status, body) = emulator.fetch(&url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: fetching the published URL must open a session, got {status} {body:?}"
        );
        let session: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("case {case:?}: the session must be JSON, got {body:?}: {error}")
        });
        assert_eq!(
            str_field(&session, "ResourceArn"),
            arn,
            "case {case:?}: the session must name the server it belongs to, got {session}"
        );
        let expected_expiry = grant
            .session_expires_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("a session expiry after the epoch")
            .as_secs_f64();
        let reported = session
            .get("SessionExpiresAt")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        // The instant travels as a JSON number, so the comparison allows the rounding a
        // decimal rendering of an f64 introduces — a millisecond, where the windows under
        // test are half an hour and longer.
        assert!(
            (reported - expected_expiry).abs() < 0.001,
            "case {case:?}: the session must end when the grant says ({expected_expiry}), got \
             {reported}"
        );
    }
}

/// RULE: the URL authenticates only while BOTH windows the mint recorded are still open —
/// the URL's own `ExpiresInSeconds` validity and the session's
/// `SessionExpirationDurationInSeconds` — so one the clock has closed stops
/// authenticating, whatever the server's own status still reports.
#[tokio::test]
async fn a_url_authenticates_only_until_the_window_it_was_minted_with_has_elapsed() {
    // Rows differ along which window closes first and how long each is: the published
    // bounds of `ExpiresInSeconds` (5..300), a value between them, the emulator's default
    // when the request names none, and a session window shorter than the URL's own.
    let cases: Vec<(&str, Value, u64, u64)> = vec![
        ("expiry-at-the-published-minimum", json!({ "ExpiresInSeconds": 5 }), 5, 1_800),
        ("expiry-between-the-bounds", json!({ "ExpiresInSeconds": 120 }), 120, 1_800),
        (
            "expiry-at-the-published-maximum",
            json!({ "ExpiresInSeconds": 300, "SessionExpirationDurationInSeconds": 43_200 }),
            300,
            43_200,
        ),
        ("no-validity-members", json!({}), 300, 1_800),
        (
            "session-shorter-than-the-url-window",
            json!({ "ExpiresInSeconds": 300, "SessionExpirationDurationInSeconds": 1_800 }),
            300,
            1_800,
        ),
    ];

    for (case, validity, url_seconds, session_seconds) in &cases {
        let emulator = Emulator::new();
        let name = format!("ttl-{}", url_seconds * 10 + session_seconds % 7);
        create_in(&emulator, case, "us-east-1", &minimal(&name)).await;

        let mut body = validity.clone();
        body["TrackingServerName"] = json!(name.clone());
        let (status, parsed) = emulator
            .call("CreatePresignedMlflowTrackingServerUrl", &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {parsed:?}"
        );
        let url = str_field(&parsed, "AuthorizedUrl");
        auth_token(case, &url, &name);

        // The control: a fresh URL authenticates.
        let (status, served) = emulator.fetch(&url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a fresh URL must authenticate, got {status} {served:?}"
        );

        // The clock, driven rather than waited for: one second short of the shorter of the
        // two windows, the URL still authenticates.
        let shorter = (*url_seconds).min(*session_seconds);
        backdate_grants(&emulator, &name, Duration::from_secs(shorter - 1));
        let (status, served) = emulator.fetch(&url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: inside the {shorter}s window the URL must authenticate, got \
             {status} {served:?}"
        );

        // One second past it, it authenticates nothing.
        backdate_grants(&emulator, &name, Duration::from_secs(2));
        let (status, served) = emulator.fetch(&url).await;
        assert_eq!(
            status, 403,
            "case {case:?}: past the {shorter}s window the URL must authenticate nothing, got \
             {status} {served:?}"
        );
    }
}

/// RULE: only the grant a mint recorded authenticates a session — a token nothing minted,
/// a tampered one, an absent one and another server's own are each refused, and so is the
/// address of a server this account holds nothing under.
#[tokio::test]
async fn only_a_recorded_grant_authenticates_a_tracking_server_session() {
    let emulator = Emulator::new();
    let name = "grant-check";
    let other = "grant-check-other";
    create_in(&emulator, "subject", "us-east-1", &minimal(name)).await;
    create_in(&emulator, "other", "us-east-1", &minimal(other)).await;

    let minted = emulator
        .ok(
            "CreatePresignedMlflowTrackingServerUrl",
            &json!({ "TrackingServerName": name }),
        )
        .await;
    let url = str_field(&minted, "AuthorizedUrl");
    let encoded = auth_token("subject", &url, name);
    let other_minted = emulator
        .ok(
            "CreatePresignedMlflowTrackingServerUrl",
            &json!({ "TrackingServerName": other }),
        )
        .await;
    let other_token = auth_token("other", &str_field(&other_minted, "AuthorizedUrl"), other);

    let prefix = expected_prefix(name);
    let mut tampered: Vec<char> = encoded.chars().collect();
    if let Some(first) = tampered.first_mut() {
        *first = if *first == 'A' { 'B' } else { 'A' };
    }
    let tampered: String = tampered.into_iter().collect();
    // Rows differ along HOW the presented token fails.
    let attempts: Vec<(&str, String)> = vec![
        ("a token nothing minted", format!("{prefix}not-a-grant")),
        ("a tampered token", format!("{prefix}{tampered}")),
        (
            "no token at all",
            prefix.trim_end_matches("?authToken=").to_string(),
        ),
        ("another server's token", format!("{prefix}{other_token}")),
        (
            "a server this account does not hold",
            format!("{}{encoded}", expected_prefix("grant-check-absent")),
        ),
    ];
    for (attempt, address) in &attempts {
        let (status, body) = emulator.fetch(address).await;
        assert_eq!(
            status, 403,
            "case {attempt:?}: expected HTTP 403 for {address:?}, got {status} {body:?}"
        );
    }

    // The grant itself still authenticates: a refused attempt consumed nothing.
    let (status, body) = emulator.fetch(&url).await;
    assert_eq!(
        status, 200,
        "case recorded-grant-after-refusals: the grant must still authenticate, got {status} \
         {body:?}"
    );
}

/// RULE: a name the account holds no tracking server under has nothing to presign — the
/// modelled `ResourceNotFound` at HTTP 400, worded the way this service words a missing
/// tracking server, with the ARN the request's name and Region resolve to — and the
/// refusal brings no server into being.
#[tokio::test]
async fn presigning_a_name_no_server_was_created_under_is_refused_with_resource_not_found() {
    let emulator = Emulator::new();
    create_in(&emulator, "existing", "us-east-1", &minimal("presign-exists")).await;

    // Rows: a name never created, one differing from a stored name only in case, one a
    // character short, and the longest legal name — each in a Region of its own, since the
    // refusal names the ARN the signed Region resolves to.
    let longest = format!("z{}", "y".repeat(255));
    let cases: Vec<(&str, &str, String)> = vec![
        ("never-created", "us-east-1", "absent".to_string()),
        (
            "case-differs-from-a-stored-name",
            "eu-west-3",
            "Presign-Exists".to_string(),
        ),
        ("one-character-off", "ap-southeast-2", "presign-exist".to_string()),
        ("longest-legal-name", "us-gov-west-1", longest),
    ];

    for (case, region, name) in &cases {
        let (status, parsed) = emulator
            .call_in(
                region,
                "CreatePresignedMlflowTrackingServerUrl",
                &json!({ "TrackingServerName": name }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {case:?}: expected the modelled ResourceNotFound, got {parsed:?}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "Could not find mlflow tracking server \
                 \"arn:aws:sagemaker:{region}:{ACCOUNT}:mlflow-tracking-server/{name}\"."
            ),
            "case {case:?}: the refusal must name the ARN the request resolves to, got {:?}",
            message(&parsed)
        );
        assert!(
            parsed.get("AuthorizedUrl").is_none(),
            "case {case:?}: a refused call presigns nothing, got {parsed:?}"
        );
        assert!(
            grants(&emulator, name).is_empty(),
            "case {case:?}: a refused call records no grant"
        );
    }

    // The refusals changed nothing: the server that does exist still presigns.
    let minted = emulator
        .ok(
            "CreatePresignedMlflowTrackingServerUrl",
            &json!({ "TrackingServerName": "presign-exists" }),
        )
        .await;
    auth_token(
        "existing-after-refusals",
        &str_field(&minted, "AuthorizedUrl"),
        "presign-exists",
    );
}

/// RULE: the two validity members are accepted anywhere inside the ranges the model
/// publishes (`ExpiresInSeconds` 5..300, `SessionExpirationDurationInSeconds`
/// 1800..43200) and refused one step outside them, before the handler runs — and neither
/// changes the answer's shape, which carries the one member the model declares.
#[tokio::test]
async fn the_validity_members_are_accepted_inside_their_published_ranges_and_refused_outside() {
    let emulator = Emulator::new();
    let name = "presign-validity";
    create_in(&emulator, "validity", "us-east-1", &minimal(name)).await;

    // Accepted rows: both published bounds of each range and a value between them.
    let accepted: Vec<(&str, Value)> = vec![
        ("expiry-at-the-published-minimum", json!({ "ExpiresInSeconds": 5 })),
        ("expiry-between-the-bounds", json!({ "ExpiresInSeconds": 123 })),
        ("expiry-at-the-published-maximum", json!({ "ExpiresInSeconds": 300 })),
        (
            "session-at-the-published-minimum",
            json!({ "SessionExpirationDurationInSeconds": 1_800 }),
        ),
        (
            "session-between-the-bounds",
            json!({ "SessionExpirationDurationInSeconds": 20_000 }),
        ),
        (
            "session-at-the-published-maximum",
            json!({ "SessionExpirationDurationInSeconds": 43_200 }),
        ),
    ];
    for (case, validity) in &accepted {
        let mut body = validity.clone();
        body["TrackingServerName"] = json!(name);
        let (status, parsed) = emulator
            .call("CreatePresignedMlflowTrackingServerUrl", &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a value inside the published range is accepted, got {status} with \
             {parsed:?}"
        );
        auth_token(case, &str_field(&parsed, "AuthorizedUrl"), name);
    }

    // Refused rows: one step past each published bound, plus zero and a negative.
    let refused: Vec<(&str, Value)> = vec![
        ("expiry-one-below-the-minimum", json!({ "ExpiresInSeconds": 4 })),
        ("expiry-one-above-the-maximum", json!({ "ExpiresInSeconds": 301 })),
        ("expiry-zero", json!({ "ExpiresInSeconds": 0 })),
        ("expiry-negative", json!({ "ExpiresInSeconds": -1 })),
        (
            "session-one-below-the-minimum",
            json!({ "SessionExpirationDurationInSeconds": 1_799 }),
        ),
        (
            "session-one-above-the-maximum",
            json!({ "SessionExpirationDurationInSeconds": 43_201 }),
        ),
        (
            "session-zero",
            json!({ "SessionExpirationDurationInSeconds": 0 }),
        ),
    ];
    for (case, validity) in &refused {
        let mut body = validity.clone();
        body["TrackingServerName"] = json!(name);
        let (status, parsed) = emulator
            .call("CreatePresignedMlflowTrackingServerUrl", &body)
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the published range is refused, got {status} with \
             {parsed:?}"
        );
        assert!(
            parsed.get("AuthorizedUrl").is_none(),
            "case {case:?}: a refused call presigns nothing, got {parsed:?}"
        );
    }
}

/// RULE: minting a presigned URL is not a change to the tracking server — a later
/// `DescribeMlflowTrackingServer` reports exactly what it would have reported anyway, and
/// the minted token is retained in nothing it reports.
#[tokio::test]
async fn presigning_changes_nothing_a_later_describe_reports() {
    let emulator = Emulator::new();
    let name = "presign-stable";
    create_in(&emulator, "stability", "us-east-1", &minimal(name)).await;

    // A presign while the server is still coming up is served: the URL is how the caller
    // reaches the MLflow UI, and the server is held whatever status it is in.
    let minted = emulator
        .ok(
            "CreatePresignedMlflowTrackingServerUrl",
            &json!({ "TrackingServerName": name }),
        )
        .await;
    auth_token("presign-while-creating", &str_field(&minted, "AuthorizedUrl"), name);

    // The `Creating` -> `Created` transition the clock owes this server, brought due by
    // DRIVING the clock rather than waiting for it: the instant it falls due is moved into
    // the past, so the next read materialises exactly what it would have materialised
    // anyway and the test waits on nothing.
    due_now(&emulator, name);
    let before = emulator
        .ok(
            "DescribeMlflowTrackingServer",
            &json!({ "TrackingServerName": name }),
        )
        .await;
    assert_eq!(
        str_field(&before, "TrackingServerStatus"),
        "Created",
        "case settled: the server must have settled, got {before:?}"
    );

    // Rows: repeated presigns, with and without the validity members, each followed by a
    // read of the settled server.
    let bodies = [
        json!({ "TrackingServerName": name }),
        json!({ "TrackingServerName": name, "ExpiresInSeconds": 5 }),
        json!({ "TrackingServerName": name, "SessionExpirationDurationInSeconds": 1_800 }),
        json!({
            "TrackingServerName": name,
            "ExpiresInSeconds": 300,
            "SessionExpirationDurationInSeconds": 43_200,
        }),
    ];
    for (index, body) in bodies.iter().enumerate() {
        let case = format!("presign-{index}");
        let minted = emulator
            .ok("CreatePresignedMlflowTrackingServerUrl", body)
            .await;
        let url = str_field(&minted, "AuthorizedUrl");
        auth_token(&case, &url, name);
        let after = emulator
            .ok(
                "DescribeMlflowTrackingServer",
                &json!({ "TrackingServerName": name }),
            )
            .await;
        assert_eq!(
            after, before,
            "case {case:?}: the server must read back identically, got {after:?}"
        );
        assert!(
            !after.to_string().contains(&token_of(&url)),
            "case {case:?}: the minted token must be retained in no reported state, got {after:?}"
        );
    }
}

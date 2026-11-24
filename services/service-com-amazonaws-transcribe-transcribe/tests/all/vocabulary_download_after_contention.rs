// SPDX-License-Identifier: CC0-1.0

//! WHOSE content the `DownloadUri` of a custom vocabulary serves once several creates
//! have CONTENDED for one name, end to end over the wire on an emulator that verifies
//! every request: N concurrent `CreateVocabulary` calls on one name, then the family's
//! reader, then a fetch of the address that reader reported made the way a client makes
//! it — the address exactly as it was handed back, with no credential of the client's
//! own.
//!
//! THE RULE THESE ROWS ASSERT: the location a reader reports for a resource in this
//! service's own store is a temporary PRE-AUTHORIZED address of the file this service
//! stored for that one resource — "use this URI to view or download the custom
//! vocabulary" (`smithy_query(query="operation", operation="GetVocabulary")`), and the
//! model's own word for such a location is a "temporary URI you can use for secure
//! access" that is "only valid for 15 minutes" (`smithy_query(query="doc",
//! shape="Transcript", member="TranscriptFileUri")`) — so a fetch made while that grant
//! is still open is served HTTP 200 and the resource's CURRENT content. Where creates
//! contended for the name, that content is the content of the create that won the name
//! and of no other: a create refused the name stores nothing, so none of its terms can
//! reach the file. A refusal of such a fetch is not in the reader's modelled error set
//! at all, and the only refusal the member's own documentation admits is an expired
//! grant, which a fetch made right after the read is not.
//!
//! The rows differ along what the rule generalises over: HOW MANY creates contend for
//! the name (two, three, four and eight), how many terms each one would store (a single
//! phrase, equal lists, lists of differing lengths), which Region serves the race — and
//! so which Region's store holds the winner's file — and the shape of the name they
//! contend for (dotted, underscored, hyphenated, mixed case). WHICH racer wins is not
//! chosen by the suite: it is read off the answers, and the content expected at the
//! location follows from it.
//!
//! WHAT a reported location is (a temporary pre-signed address on this emulator's own
//! endpoint, and the one thing that authorises its fetch) is
//! `crate::served_download_locations`' subject, and WHAT one serves after a create, an
//! update and a delete is `crate::vocabulary_download_content`'s; the race's own
//! control-plane invariant — one winner, every other racer the modelled conflict — is
//! `crate::vocabularies`'. This suite is the one that puts the two together: it reads
//! the content the race settled on back out of the location, which is the only way a
//! client can observe which writer won and that no loser's terms leaked in.
//!
//! Request authentication is ON here (the `--auth on` path the emulator binary runs),
//! so every control-plane call is one the emulator verifies and the fetch rests on
//! nothing but the signature the reported address carries. The emulator mounted holds
//! the Amazon S3 service beside Transcribe on ONE shared engine, which is what the
//! assembled router does: the file is stored by calling object storage in process, so a
//! suite that fetches it needs object storage answering.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_transcribe_transcribe::state;

use crate::served_download_locations::{fetch, named_object};

/// How long a poll loop keeps looking before it gives up, and how long it waits between
/// two looks. Generous: what is waited for is a build transition this service owes in
/// well under a second, and nothing here is timed — the loop stops as soon as the
/// condition holds.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
/// See [`POLL_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long the signature this suite puts on a control-plane call stays valid, in
/// seconds: far wider than any row can take, so no row's outcome depends on how fast
/// the host ran.
const REQUEST_SIGNATURE_VALIDITY: u64 = 60 * 60;

/// The state whose documentation makes a custom vocabulary usable, and the only one a
/// `DownloadUri` is reported in.
const READY: &str = "READY";

/// The build state a vocabulary is in until the service has finished processing it.
const PENDING: &str = "PENDING";

/// The kind of resource this service names in the refusal of a create whose name a
/// record already holds, as `state::name_already_exists_message` words it.
const KIND_VOCABULARY: &str = "vocabulary";

/// One distinct marker per racer, so the file served after a race says WHICH create's
/// content persisted: every phrase of racer `r` carries `MARKERS[r]`, and no other
/// racer's phrase does. Letters only, as a custom vocabulary's terms are.
const MARKERS: [&str; 8] = [
    "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
];

/// The words a racer's marker is combined with to make its phrase list.
const TERMS: [&str; 5] = ["Aardvark", "Zebra", "Gnu", "Ocelot", "Narwhal"];

/// One race for a name: how many creates contend for it, what each would store, and
/// where it is served.
struct Row {
    /// What this row is, named in every assertion it fails.
    case: &'static str,
    /// The Region the row's calls are signed for, which is the Region whose store holds
    /// the winner's file.
    region: &'static str,
    /// The name every racer of the row contends for.
    name: &'static str,
    /// How many creates contend for it; at most one per entry of [`MARKERS`].
    racers: usize,
    /// How many phrases racer `r` would store: `phrase_counts[r % phrase_counts.len()]`,
    /// so a row can have its racers claim lists of equal or of differing lengths.
    phrase_counts: &'static [usize],
}

/// The races this rule is asserted over (see this module's header for the dimensions).
/// Each row's name is a different spelling the model's `VocabularyName` `@pattern`
/// admits (`^[0-9a-zA-Z._-]+$`), and each is served in its own Region so the rows
/// cannot see one another's records.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "two creates contend, one phrase each, in the default Region",
            region: "us-east-1",
            name: "contended.two",
            racers: 2,
            phrase_counts: &[1],
        },
        Row {
            case: "three creates contend with lists of differing lengths, eu-west-1",
            region: "eu-west-1",
            name: "contended_three",
            racers: 3,
            phrase_counts: &[1, 2, 5],
        },
        Row {
            case: "four creates contend, several phrases each, us-west-2",
            region: "us-west-2",
            name: "Contended4",
            racers: 4,
            phrase_counts: &[3],
        },
        Row {
            case: "eight creates contend, one phrase each, ap-northeast-1",
            region: "ap-northeast-1",
            name: "contended-eight",
            racers: 8,
            phrase_counts: &[1],
        },
    ]
}

/// The phrases racer `racer` of `row` would store: its own marker on each term, so no
/// two racers of a row share a phrase and the file served after the race names the
/// create whose content it holds.
fn racer_phrases(row: &Row, racer: usize) -> Vec<String> {
    assert!(
        row.racers <= MARKERS.len(),
        "case {:?}: a row has at most one racer per marker, got {}",
        row.case,
        row.racers
    );
    let marker = MARKERS[racer];
    let count = row.phrase_counts[racer % row.phrase_counts.len()];
    (0..count)
        .map(|index| format!("{marker}{}", TERMS[index % TERMS.len()]))
        .collect()
}

/// An emulator with request authentication ON — the `--auth on` path the emulator binary
/// runs — holding Transcribe and Amazon S3 on one shared engine, together with the key
/// pair it published for this run.
struct Emulator {
    router: Arc<MultiServiceRouter>,
    access_key_id: String,
    secret_access_key: String,
}

impl Emulator {
    /// Behind an `Arc`, so each racer of a race can be handed the same emulator.
    fn new() -> Arc<Self> {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(true);
        let (access_key_id, secret_access_key) = match router.vended_credentials().into_iter().next()
        {
            Some(emulator_server::VendedCredentials::AwsAccessKey {
                access_key_id,
                secret_access_key,
            }) => (access_key_id, secret_access_key),
            other => panic!(
                "with authentication on the emulator must vend a SigV4 key pair, got {other:?}"
            ),
        };
        router.register_service(
            service_com_amazonaws_transcribe_transcribe::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Arc::new(Self {
            router: Arc::new(router),
            access_key_id,
            secret_access_key,
        })
    }

    /// One Transcribe call in the service's own wire envelope, signed for `region` with
    /// the key pair this emulator published through its own presigner — so what this
    /// suite drives is a request the emulator computes the same signature for, rather
    /// than a signature written here by hand.
    ///
    /// Answers the HTTP status and the body as JSON, a refusal included: a racer that
    /// loses a contended name is answered one, and reading it is the point.
    async fn call(&self, operation: &str, input: &Value, region: &str) -> (u16, Value) {
        let endpoint = format!("https://transcribe.{region}.amazonaws.com");
        let target = format!("Transcribe.{operation}");
        let headers = [
            ("x-amz-target", target.as_str()),
            ("content-type", "application/x-amz-json-1.1"),
        ];
        let presign = emulator_server::request_authentication::PresignTarget {
            endpoint: &endpoint,
            method: "POST",
            signing_name: "transcribe",
            region,
            path: "/",
            query: &[],
            headers: &headers,
        };
        let url = emulator_server::request_authentication::presign_url(
            &presign,
            &self.access_key_id,
            &self.secret_access_key,
            SystemTime::now(),
            REQUEST_SIGNATURE_VALIDITY,
        )
        .expect("the request must be signable");
        let mut builder = ::http::Request::builder()
            .method("POST")
            .uri(url)
            .header("host", endpoint.trim_start_matches("https://").to_string());
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        // The header a presigned request to a service outside the S3 family must send
        // for its body to verify, which the presigner signed in.
        if let Some((name, value)) =
            emulator_server::request_authentication::presigned_payload_header("transcribe", "POST")
        {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(::bytes::Bytes::from(
                serde_json::to_vec(input).expect("serializable input"),
            ))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = response.body().as_ref().to_vec();
        let body = if answered.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&answered).unwrap_or_else(|error| {
                panic!(
                    "{operation} answered {status} with a body that is not JSON ({error}): {:?}",
                    String::from_utf8_lossy(&answered)
                )
            })
        };
        (status, body)
    }

    /// One call that must be served, answering its body.
    async fn served(&self, case: &str, operation: &str, input: &Value, region: &str) -> Value {
        let (status, body) = self.call(operation, input, region).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must be served to a verified caller, got HTTP \
             {status} with body {body:?}"
        );
        body
    }

    /// Poll `GetVocabulary` until the build of `name` has FINISHED, and answer the
    /// reader's whole answer. The wait is for the STATE the clock owes the build, looked
    /// for again until it holds, never for a span of the wall clock.
    async fn settled_vocabulary(&self, case: &str, name: &str, region: &str) -> Value {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let read = self
                .served(case, "GetVocabulary", &json!({"VocabularyName": name}), region)
                .await;
            if read["VocabularyState"] != json!(PENDING) {
                return read;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the build of {name:?} never finished"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// The awsJson error code an answer names (`__type`), or `""` when it carries none.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .map_or("", |code| code.rsplit(['#', ':']).next().unwrap_or(""))
}

/// The message an error body carries, whichever casing the envelope spells it in.
fn error_message(case: &str, body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message in {body:?}"))
        .to_string()
}

/// The location the reader reports, or a panic naming what reported none.
fn download_uri(case: &str, read: &Value) -> String {
    read.get("DownloadUri")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a built custom vocabulary reports a DownloadUri, got {read:?}")
        })
        .to_string()
}

/// Run every racer of `row` at once, and answer each one's racer index, HTTP status and
/// body.
async fn race(emulator: &Arc<Emulator>, row: &Row) -> Vec<(usize, u16, Value)> {
    assert!(
        row.racers <= MARKERS.len(),
        "case {:?}: a row has at most one racer per marker, got {}",
        row.case,
        row.racers
    );
    let mut tasks = Vec::new();
    for (racer, marker) in MARKERS.iter().enumerate().take(row.racers) {
        let emulator = Arc::clone(emulator);
        let region = row.region;
        let request = json!({
            "VocabularyName": row.name,
            "LanguageCode": "en-US",
            "Phrases": racer_phrases(row, racer),
            "Tags": [{"Key": "racer", "Value": marker}],
        });
        tasks.push(tokio::spawn(async move {
            let (status, body) = emulator.call("CreateVocabulary", &request, region).await;
            (racer, status, body)
        }));
    }
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.expect("the racer ran"));
    }
    outcomes
}

/// Run `row`'s race, assert exactly one create won the name and that every other racer
/// was refused with one of the two sentences this service words a taken name with, and
/// answer the winner's racer index.
async fn sole_winner(emulator: &Arc<Emulator>, row: &Row) -> usize {
    let case = row.case;
    let outcomes = race(emulator, row).await;
    let winners: Vec<usize> = outcomes
        .iter()
        .filter(|(_, status, _)| *status == 200)
        .map(|(racer, _, _)| *racer)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "case {case:?}: exactly one create of {:?} may win the name, got {outcomes:?}",
        row.name
    );
    let winner = winners[0];

    // Every other racer is refused the name. The two sentences are this service's own:
    // a create that collides with a record already there gets that family's sentence,
    // one that loses the name to a create still being served gets the generic one, and
    // which of the two a racer meets depends on how far the winner had got.
    let held = state::name_already_exists_message(KIND_VOCABULARY);
    let claimed = state::contended_name_claim_message();
    for (racer, status, body) in outcomes.iter().filter(|(racer, _, _)| *racer != winner) {
        let case = format!("{case} (racer {racer} lost the name)");
        // Every client error of this awsJson1_1 service answers HTTP 400 and names its
        // shape in `__type` (`crate::client_error_wire_status` pins that mapping).
        assert_eq!(
            *status, 400,
            "case {case:?}: a create refused a taken name is answered a client error, \
             got HTTP {status} with body {body:?}"
        );
        assert_eq!(
            error_code(body),
            "ConflictException",
            "case {case:?}: expected the modelled conflict, got {body:?}"
        );
        let message = error_message(&case, body);
        assert!(
            message == held || message == claimed,
            "case {case:?}: expected one of {held:?} or {claimed:?}, got {message:?}"
        );
    }
    winner
}

/// Fetch `uri` the way a client follows a location it was handed, assert it is served,
/// and answer the terms the file lists.
async fn served_terms(emulator: &Arc<Emulator>, case: &str, uri: &str) -> Vec<String> {
    let (status, body) = fetch(&emulator.router, uri).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the reported DownloadUri {uri:?} must serve the stored file to a \
         client carrying no credential of its own, got HTTP {status} with body {:?}",
        String::from_utf8_lossy(&body)
    );
    String::from_utf8_lossy(&body)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The content the create that WON a contended name stored is what the location the
/// reader reports serves — and none of the losers' terms is there.
#[tokio::test(flavor = "multi_thread")]
async fn the_location_serves_the_content_of_the_create_that_won_the_name() {
    let emulator = Emulator::new();
    for row in &rows() {
        let case = row.case;
        let winner = sole_winner(&emulator, row).await;

        let read = emulator
            .settled_vocabulary(case, row.name, row.region)
            .await;
        assert_eq!(
            read["VocabularyState"],
            json!(READY),
            "case {case:?}: a vocabulary built from its request's own phrases must settle \
             {READY}, got {read:?}"
        );
        let uri = download_uri(case, &read);
        // A modelled `Uri` (@length 1..2000, @pattern ^(s3://|http(s*)://).+$) naming
        // this resource's own object.
        assert!(
            uri.starts_with("https://") || uri.starts_with("http://"),
            "case {case:?}: a reported location is an address a client can fetch, got {uri:?}"
        );
        assert!(
            uri.len() <= 2_000,
            "case {case:?}: a reported location is a modelled Uri, got {} characters",
            uri.len()
        );
        assert!(
            named_object(&uri).ends_with(row.name),
            "case {case:?}: the address must name this vocabulary's own object, got {uri:?}"
        );

        let expected = racer_phrases(row, winner);
        let served = served_terms(&emulator, case, &uri).await;
        assert_eq!(
            served, expected,
            "case {case:?}: the location must serve the phrases the winning create \
             (racer {winner}) stored, expected {expected:?}, got {served:?}"
        );

        // No loser's terms leaked in: a create refused the name stored nothing, so the
        // file cannot hold a phrase of one.
        for loser in (0..row.racers).filter(|racer| *racer != winner) {
            for phrase in racer_phrases(row, loser) {
                assert!(
                    !served.contains(&phrase),
                    "case {case:?}: racer {loser} was refused the name, so {phrase:?} \
                     must not be served, got {served:?}"
                );
            }
        }

        // And the name is held by exactly ONE record, so there is one location to read.
        let listed = emulator
            .served(
                case,
                "ListVocabularies",
                &json!({"NameContains": row.name}),
                row.region,
            )
            .await;
        let names: Vec<String> = listed["Vocabularies"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry["VocabularyName"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names,
            vec![row.name.to_string()],
            "case {case:?}: a contended name is held by exactly one record, got {names:?}"
        );
    }
}

/// EVERY read hands back a location that is fetchable right away: the address is
/// temporary, so each read mints its own over the same object, and a fetch made while
/// that grant is open is served the same stored content.
#[tokio::test(flavor = "multi_thread")]
async fn every_read_after_a_contended_create_hands_back_a_fetchable_location() {
    let emulator = Emulator::new();
    let row = Row {
        case: "eight creates contend, then the winner's location is read twice",
        region: "us-east-1",
        name: "contended.read-again",
        racers: 8,
        phrase_counts: &[2, 4],
    };
    let winner = sole_winner(&emulator, &row).await;
    let expected = racer_phrases(&row, winner);

    let mut addresses = Vec::new();
    for pass in ["the first read", "the second read"] {
        let case = format!("{} ({pass})", row.case);
        let read = emulator
            .settled_vocabulary(&case, row.name, row.region)
            .await;
        let uri = download_uri(&case, &read);
        let served = served_terms(&emulator, &case, &uri).await;
        assert_eq!(
            served, expected,
            "case {case:?}: every read's location serves the winning create's phrases, \
             expected {expected:?}, got {served:?}"
        );
        addresses.push(uri);
    }
    let objects: Vec<&str> = addresses.iter().map(|uri| named_object(uri)).collect();
    assert_eq!(
        objects[0], objects[1],
        "case {:?}: two reads of one record name the same object, got {addresses:?}",
        row.case
    );
}

// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the archive an export job hands out: what `CreateExport`
//! assembles, what `DescribeExport` announces for it, and what the address it announces
//! serves.
//!
//! Every request goes through the same `MultiServiceRouter::dispatch` the emulator serves
//! on its socket — the modelled operations and the archive's own pre-signed address alike —
//! so the routing, the media types and the bytes are exercised exactly as a client sees
//! them. The harness, the bot, the locales, the intents, the slot types and the slots are
//! the other suites', reused rather than copied.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one case:
//!
//! * an export job assembles a zip archive out of the stored definition of the resource its
//!   `resourceSpecification` names — the bot, the locales of the version exported, each
//!   locale's intents and slot types, and each intent's slots — so what the archive holds is
//!   what a read of those resources reports, and an export of one locale holds that locale
//!   alone;
//! * a job that has reached the status in which the export is ready to download announces
//!   the pre-signed address its archive is served at, and that address serves those very
//!   bytes;
//! * the archive is the SNAPSHOT of the instant the job was recorded: a later change to the
//!   bot does not reach a job already submitted;
//! * a definition file carries the VALUES the resource it describes was created with —
//!   whatever threshold the locale was given, whatever the intent is called and however many
//!   sample utterances it holds — and the job that assembled it reports no reason for a
//!   failure that did not happen.

use std::io::{Cursor, Read};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lexmodelsv2_lexmodelbuildingservicev2::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::bot_lifecycle::{call, create_bot, create_body, emulator, member, text};
use crate::bot_locale::{locale_body, locales_uri};
use crate::intents_and_builds::{create_intent, intent_body};
use crate::slot_type::{create_slot_type, slot_type_body};
use crate::slots::{slot_body, slots_uri};

/// `CreateExport`'s URI (`PUT /exports`).
const EXPORTS_URI: &str = "/exports";

/// How long a poll for a due status transition may run before the test gives up. Generous
/// on purpose: the loop waits for the CONDITION, and only a host that never applies the
/// transition at all runs it out.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The locale the fixture bot fills with intents, slot types and slots.
const FILLED_LOCALE: &str = "en_US";

/// A second locale of the same bot, carrying only the intent Amazon Lex adds to a locale as
/// it creates it: an export of one locale must hold that locale alone.
const BARE_LOCALE: &str = "de_DE";

/// The intent the fixture creates in [`FILLED_LOCALE`], and its one sample utterance.
const INTENT_NAME: &str = "OrderFlowers";

/// See [`INTENT_NAME`].
const INTENT_UTTERANCE: &str = "I would like to order flowers";

/// The custom slot type the fixture creates in [`FILLED_LOCALE`].
const SLOT_TYPE_NAME: &str = "FlowerTypes";

/// The slot the fixture creates on [`INTENT_NAME`].
const SLOT_NAME: &str = "FlowerType";

/// The bot, the locales, the intent, the slot type and the slot one case is exported from.
struct Fixture {
    bot_id: String,
    bot_name: String,
    /// The numbered version taken from the filled locale.
    bot_version: String,
    intent_id: String,
    slot_type_id: String,
}

/// Create the bot every case exports: two locales, one of them carrying an intent with a
/// sample utterance, a custom slot type and a slot of that intent, plus a numbered version
/// taken from that locale.
async fn fixture(router: &MultiServiceRouter, bot_name: &str) -> Fixture {
    let bot_id = create_bot(router, &create_body(bot_name, 300, json!({}))).await;
    for locale_id in [FILLED_LOCALE, BARE_LOCALE] {
        let (status, created) = call(
            router,
            "PUT",
            &locales_uri(&bot_id, "DRAFT"),
            Some(locale_body(locale_id, 0.4, json!({}))),
        )
        .await;
        assert_eq!(
            status, 202,
            "locale {locale_id}: expected CreateBotLocale to answer 202, got {status} \
             {created:?}"
        );
    }
    let slot_type_id = create_slot_type(
        router,
        &bot_id,
        FILLED_LOCALE,
        &slot_type_body(
            SLOT_TYPE_NAME,
            json!({ "slotTypeValues": [{ "sampleValue": { "value": "roses" } }] }),
        ),
    )
    .await;
    let intent_id = create_intent(
        router,
        &bot_id,
        FILLED_LOCALE,
        &intent_body(
            INTENT_NAME,
            json!({ "sampleUtterances": [{ "utterance": INTENT_UTTERANCE }] }),
        ),
    )
    .await;
    let (status, slot) = call(
        router,
        "PUT",
        &slots_uri(&bot_id, "DRAFT", FILLED_LOCALE, &intent_id),
        Some(slot_body(
            SLOT_NAME,
            json!({ "slotTypeId": slot_type_id.clone() }),
        )),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected CreateSlot to answer 200, got {status} {slot:?}"
    );
    let (status, version) = call(
        router,
        "PUT",
        &format!("/bots/{bot_id}/botversions"),
        Some(json!({
            "botVersionLocaleSpecification": { FILLED_LOCALE: { "sourceBotVersion": "DRAFT" } },
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "expected CreateBotVersion to answer 202, got {status} {version:?}"
    );
    Fixture {
        bot_id,
        bot_name: bot_name.to_string(),
        bot_version: text(&version, "botVersion"),
        intent_id,
        slot_type_id,
    }
}

/// One row of the archive table: what is exported, in which format, and the archive it
/// must assemble.
struct Case {
    case: &'static str,
    /// The `resourceSpecification` of the `CreateExport` request.
    specification: Value,
    /// The `fileFormat` asked for, which the archive's manifest states back.
    file_format: &'static str,
    /// The kind of resource, as the manifest and the `ExportResourceType` filter name it.
    resource_type: &'static str,
    /// Whether the archive carries the bot's own definition file.
    with_bot_definition: bool,
    /// The locales the archive carries, in the order it carries them.
    locales: Vec<&'static str>,
    /// The bot version exported, which the locales' intents are read from.
    bot_version: String,
}

/// The table every assertion below runs over: the rows differ in the kind of resource
/// exported, in the bot version it is taken from, in which locale it names, in the file
/// format asked for, and in how much the exported locale holds.
fn cases(fixture: &Fixture) -> Vec<Case> {
    let bot_id = fixture.bot_id.clone();
    let numbered = fixture.bot_version.clone();
    vec![
        Case {
            case: "a whole bot at its draft version",
            specification: json!({
                "botExportSpecification": { "botId": bot_id, "botVersion": "DRAFT" },
            }),
            file_format: "LexJson",
            resource_type: state::EXPORT_RESOURCE_TYPE_BOT,
            with_bot_definition: true,
            locales: vec![BARE_LOCALE, FILLED_LOCALE],
            bot_version: "DRAFT".to_string(),
        },
        Case {
            case: "a whole bot at a numbered version",
            specification: json!({
                "botExportSpecification": { "botId": bot_id, "botVersion": numbered },
            }),
            file_format: "LexJson",
            resource_type: state::EXPORT_RESOURCE_TYPE_BOT,
            with_bot_definition: true,
            // The version was taken from the filled locale alone, so that is the only
            // locale the version holds.
            locales: vec![FILLED_LOCALE],
            bot_version: numbered.clone(),
        },
        Case {
            case: "the filled locale of the draft",
            specification: json!({
                "botLocaleExportSpecification": {
                    "botId": bot_id,
                    "botVersion": "DRAFT",
                    "localeId": FILLED_LOCALE,
                },
            }),
            file_format: "LexJson",
            resource_type: state::EXPORT_RESOURCE_TYPE_BOT_LOCALE,
            with_bot_definition: false,
            locales: vec![FILLED_LOCALE],
            bot_version: "DRAFT".to_string(),
        },
        Case {
            case: "the locale of the draft that holds only its fallback intent",
            specification: json!({
                "botLocaleExportSpecification": {
                    "botId": bot_id,
                    "botVersion": "DRAFT",
                    "localeId": BARE_LOCALE,
                },
            }),
            file_format: "TSV",
            resource_type: state::EXPORT_RESOURCE_TYPE_BOT_LOCALE,
            with_bot_definition: false,
            locales: vec![BARE_LOCALE],
            bot_version: "DRAFT".to_string(),
        },
        Case {
            case: "the filled locale of a numbered version",
            specification: json!({
                "botLocaleExportSpecification": {
                    "botId": bot_id,
                    "botVersion": numbered,
                    "localeId": FILLED_LOCALE,
                },
            }),
            file_format: "CSV",
            resource_type: state::EXPORT_RESOURCE_TYPE_BOT_LOCALE,
            with_bot_definition: false,
            locales: vec![FILLED_LOCALE],
            bot_version: numbered.clone(),
        },
    ]
}

/// Submit one export and answer the `exportId` it was recorded under.
async fn create_export(router: &MultiServiceRouter, case: &Case) -> String {
    let body = json!({
        "resourceSpecification": case.specification,
        "fileFormat": case.file_format,
    });
    let (status, created) = call(router, "PUT", EXPORTS_URI, Some(body.clone())).await;
    assert_eq!(
        status, 202,
        "case {:?}: expected CreateExport to answer 202, got {status} {created:?}",
        case.case
    );
    text(&created, "exportId")
}

/// Read the export back until its archive is ready, or the deadline passes, and answer the
/// response that reported it ready.
///
/// The transition the clock owes is materialised by the next read, so this looks AGAIN
/// rather than waiting a fixed time for it.
async fn completed_export(router: &MultiServiceRouter, case: &str, export_id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) =
            call(router, "GET", &format!("{EXPORTS_URI}/{export_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeExport to answer 200, got {status} {described:?}"
        );
        if text(&described, "exportStatus") == state::EXPORT_STATUS_COMPLETED {
            return described;
        }
        assert_eq!(
            text(&described, "exportStatus"),
            state::EXPORT_STATUS_IN_PROGRESS,
            "case {case:?}: an export of a resource that resolved must reach the completed \
             status, got {described:?}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected export {export_id} to complete, got {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a fetch of an announced address answered.
pub(crate) struct Fetched {
    pub(crate) status: u16,
    pub(crate) media_type: String,
    pub(crate) body: Vec<u8>,
}

/// Fetch one address this emulator handed out, exactly as it was handed out.
pub(crate) async fn fetch(router: &MultiServiceRouter, url: &str) -> Fetched {
    let host = url
        .parse::<http::Uri>()
        .ok()
        .and_then(|uri| uri.authority().map(ToString::to_string))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method("GET")
        .uri(url)
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let media_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Fetched {
        status,
        media_type,
        body: response.into_body().to_vec(),
    }
}

/// Download one completed export's archive, asserting it was served as the zip archive it
/// is, and answer its bytes.
async fn archive_bytes(router: &MultiServiceRouter, case: &str, described: &Value) -> Vec<u8> {
    let url = text(described, "downloadUrl");
    assert!(
        url.starts_with("https://"),
        "case {case:?}: a completed export must announce an address for its archive, got \
         {described:?}"
    );
    let fetched = fetch(router, &url).await;
    assert_eq!(
        fetched.status, 200,
        "case {case:?}: expected the archive at {url:?}, got {} {:?}",
        fetched.status,
        String::from_utf8_lossy(&fetched.body)
    );
    assert_eq!(
        fetched.media_type, state::EXPORT_ARCHIVE_MEDIA_TYPE,
        "case {case:?}: expected the archive to be announced as {}, got {:?}",
        state::EXPORT_ARCHIVE_MEDIA_TYPE,
        fetched.media_type
    );
    fetched.body
}

/// Every entry of an archive, as `(path, text)`.
fn entries(case: &str, bytes: &[u8]) -> Vec<(String, String)> {
    let mut archive = ::zip::ZipArchive::new(Cursor::new(bytes.to_vec())).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the download must be a zip archive ({error}), got {} bytes",
            bytes.len()
        )
    });
    let mut read = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .unwrap_or_else(|error| panic!("case {case:?}: entry {index} must read ({error})"));
        let path = entry
            .name()
            .unwrap_or_else(|error| panic!("case {case:?}: entry {index} must be named ({error})"))
            .to_string();
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .unwrap_or_else(|error| panic!("case {case:?}: {path} must be UTF-8 text ({error})"));
        read.push((path, text));
    }
    read
}

/// The document one entry of an archive carries, or `None` when the archive has no such
/// entry.
fn document(entries: &[(String, String)], path: &str) -> Option<Value> {
    entries.iter().find(|(name, _)| name == path).map(|(_, text)| {
        serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("{path} must be a JSON document ({error})"))
    })
}

/// The paths the archive of `case` must carry, built from what the fixture created rather
/// than from what the archive holds.
fn expected_paths(fixture: &Fixture, case: &Case) -> Vec<String> {
    let root = &fixture.bot_name;
    let mut paths = vec![state::EXPORT_ARCHIVE_MANIFEST.to_string()];
    if case.with_bot_definition {
        paths.push(format!("{root}/{}", state::BOT_DEFINITION_FILE));
    }
    for locale_id in &case.locales {
        let locale_root = format!("{root}/{}/{locale_id}", state::BOT_LOCALES_DIRECTORY);
        paths.push(format!("{locale_root}/{}", state::BOT_LOCALE_DEFINITION_FILE));
        let intents_root = format!("{locale_root}/{}", state::INTENTS_DIRECTORY);
        paths.push(format!(
            "{intents_root}/{}/{}",
            state::FALLBACK_INTENT_NAME,
            state::INTENT_DEFINITION_FILE
        ));
        if *locale_id == FILLED_LOCALE {
            paths.push(format!(
                "{intents_root}/{INTENT_NAME}/{}",
                state::INTENT_DEFINITION_FILE
            ));
            paths.push(format!(
                "{intents_root}/{INTENT_NAME}/{}/{SLOT_NAME}/{}",
                state::SLOTS_DIRECTORY,
                state::SLOT_DEFINITION_FILE
            ));
            paths.push(format!(
                "{locale_root}/{}/{SLOT_TYPE_NAME}/{}",
                state::SLOT_TYPES_DIRECTORY,
                state::SLOT_TYPE_DEFINITION_FILE
            ));
        }
    }
    paths.sort();
    paths
}

#[tokio::test]
async fn an_export_archive_is_assembled_from_the_stored_definition_it_names() {
    let router = emulator();
    let fixture = fixture(&router, "ArchiveExportBot").await;

    // Every case is submitted first, so the span the clock owes each one runs alongside
    // the others rather than one after another.
    let cases = cases(&fixture);
    let mut submitted: Vec<(&Case, String)> = Vec::new();
    for case in &cases {
        let export_id = create_export(&router, case).await;
        submitted.push((case, export_id));
    }

    for (case, export_id) in &submitted {
        let name = case.case;
        let described = completed_export(&router, name, export_id).await;
        let bytes = archive_bytes(&router, name, &described).await;
        let entries = entries(name, &bytes);

        // The TREE: one definition file per resource the exported version holds, and
        // nothing else — an export of one locale carries that locale alone.
        let mut paths = paths_of(&entries);
        paths.sort();
        assert_eq!(
            paths,
            expected_paths(&fixture, case),
            "case {name:?}: the archive must hold one definition file per exported \
             resource, got {paths:?}"
        );

        // The MANIFEST states what the archive holds and the format it was asked for.
        let manifest = document(&entries, state::EXPORT_ARCHIVE_MANIFEST)
            .unwrap_or_else(|| panic!("case {name:?}: the archive must carry a manifest"));
        let metadata = member(&manifest, "metadata");
        assert_eq!(
            text(&metadata, "schemaVersion"),
            state::EXPORT_ARCHIVE_SCHEMA_VERSION,
            "case {name:?}: the manifest must state the schema its files are written to, \
             got {manifest:?}"
        );
        assert_eq!(
            text(&metadata, "fileFormat"),
            case.file_format,
            "case {name:?}: the manifest must state the file format asked for, got \
             {manifest:?}"
        );
        assert_eq!(
            text(&metadata, "resourceType"),
            case.resource_type,
            "case {name:?}: the manifest must state the kind of resource exported, got \
             {manifest:?}"
        );

        // The BOT's definition, on an export of a whole bot: the stored bot, at the
        // version exported.
        let bot_path = format!("{}/{}", fixture.bot_name, state::BOT_DEFINITION_FILE);
        match document(&entries, &bot_path) {
            Some(bot) => {
                assert!(
                    case.with_bot_definition,
                    "case {name:?}: an export of one locale must not carry the bot's own \
                     definition, got {bot:?}"
                );
                assert_eq!(
                    (text(&bot, "name"), text(&bot, "identifier"), text(&bot, "version")),
                    (
                        fixture.bot_name.clone(),
                        fixture.bot_id.clone(),
                        case.bot_version.clone()
                    ),
                    "case {name:?}: the bot's definition must name the stored bot and the \
                     version exported, got {bot:?}"
                );
                assert_eq!(
                    member(&bot, "idleSessionTTLInSeconds"),
                    json!(300),
                    "case {name:?}: the bot's definition must carry its stored settings, \
                     got {bot:?}"
                );
            }
            None => assert!(
                !case.with_bot_definition,
                "case {name:?}: an export of a whole bot must carry the bot's own definition"
            ),
        }

        // Each LOCALE's definition, and the intents, slots and slot types defined in it.
        for locale_id in &case.locales {
            let locale_root = format!(
                "{}/{}/{locale_id}",
                fixture.bot_name,
                state::BOT_LOCALES_DIRECTORY
            );
            let locale = document(
                &entries,
                &format!("{locale_root}/{}", state::BOT_LOCALE_DEFINITION_FILE),
            )
            .unwrap_or_else(|| panic!("case {name:?}: locale {locale_id} must be exported"));
            assert_eq!(
                text(&locale, "identifier"),
                *locale_id,
                "case {name:?}: the locale's definition must name the locale, got {locale:?}"
            );
            assert!(
                !text(&locale, "name").is_empty(),
                "case {name:?}: the locale's definition must carry the locale name the \
                 store holds, got {locale:?}"
            );
            assert_eq!(
                member(&locale, "nluConfidenceThreshold"),
                json!(0.4),
                "case {name:?}: the locale's definition must carry the stored threshold, \
                 got {locale:?}"
            );

            let fallback = document(
                &entries,
                &format!(
                    "{locale_root}/{}/{}/{}",
                    state::INTENTS_DIRECTORY,
                    state::FALLBACK_INTENT_NAME,
                    state::INTENT_DEFINITION_FILE
                ),
            )
            .unwrap_or_else(|| {
                panic!("case {name:?}: the intent Amazon Lex adds to a locale must be exported")
            });
            assert_eq!(
                text(&fallback, "parentIntentSignature"),
                state::FALLBACK_INTENT_SIGNATURE,
                "case {name:?}: the fallback intent must be exported as the built-in it is, \
                 got {fallback:?}"
            );

            if *locale_id != FILLED_LOCALE {
                continue;
            }
            let intent = document(
                &entries,
                &format!(
                    "{locale_root}/{}/{INTENT_NAME}/{}",
                    state::INTENTS_DIRECTORY,
                    state::INTENT_DEFINITION_FILE
                ),
            )
            .unwrap_or_else(|| panic!("case {name:?}: the created intent must be exported"));
            assert_eq!(
                text(&intent, "name"),
                INTENT_NAME,
                "case {name:?}: the intent's definition must name it, got {intent:?}"
            );
            assert_eq!(
                text(&intent, "identifier"),
                fixture.intent_id,
                "case {name:?}: the intent's definition must carry the id it was created \
                 under, got {intent:?}"
            );
            assert_eq!(
                member(&intent, "sampleUtterances"),
                json!([{ "utterance": INTENT_UTTERANCE }]),
                "case {name:?}: the intent's definition must carry its stored utterances, \
                 got {intent:?}"
            );

            let slot = document(
                &entries,
                &format!(
                    "{locale_root}/{}/{INTENT_NAME}/{}/{SLOT_NAME}/{}",
                    state::INTENTS_DIRECTORY,
                    state::SLOTS_DIRECTORY,
                    state::SLOT_DEFINITION_FILE
                ),
            )
            .unwrap_or_else(|| panic!("case {name:?}: the created slot must be exported"));
            assert_eq!(
                (text(&slot, "name"), text(&slot, "slotTypeId")),
                (SLOT_NAME.to_string(), fixture.slot_type_id.clone()),
                "case {name:?}: the slot's definition must carry what it elicits for, got \
                 {slot:?}"
            );
            assert_eq!(
                member(&member(&slot, "valueElicitationSetting"), "slotConstraint"),
                json!("Required"),
                "case {name:?}: the slot's definition must carry its elicitation setting, \
                 got {slot:?}"
            );

            let slot_type = document(
                &entries,
                &format!(
                    "{locale_root}/{}/{SLOT_TYPE_NAME}/{}",
                    state::SLOT_TYPES_DIRECTORY,
                    state::SLOT_TYPE_DEFINITION_FILE
                ),
            )
            .unwrap_or_else(|| panic!("case {name:?}: the created slot type must be exported"));
            assert_eq!(
                (
                    text(&slot_type, "name"),
                    text(&slot_type, "identifier"),
                    member(&slot_type, "slotTypeValues"),
                ),
                (
                    SLOT_TYPE_NAME.to_string(),
                    fixture.slot_type_id.clone(),
                    json!([{ "sampleValue": { "value": "roses" } }]),
                ),
                "case {name:?}: the slot type's definition must carry its stored values, \
                 got {slot_type:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_announced_archive_address_serves_the_same_bytes_to_every_fetch() {
    let router = emulator();
    let fixture = fixture(&router, "ArchiveAddressBot").await;
    let cases = cases(&fixture);
    let case = cases
        .first()
        .expect("the archive table holds at least one case");
    let export_id = create_export(&router, case).await;

    // While the job is still in flight there is nothing to download, so no address is
    // announced for it.
    let (status, in_flight) =
        call(&router, "GET", &format!("{EXPORTS_URI}/{export_id}"), None).await;
    assert_eq!(
        status, 200,
        "expected DescribeExport to answer 200, got {status} {in_flight:?}"
    );
    if text(&in_flight, "exportStatus") == state::EXPORT_STATUS_IN_PROGRESS {
        assert_eq!(
            member(&in_flight, "downloadUrl"),
            Value::Null,
            "an export still in progress has no archive to announce an address for, got \
             {in_flight:?}"
        );
    }

    let described = completed_export(&router, case.case, &export_id).await;
    let first = archive_bytes(&router, case.case, &described).await;

    // A read is a read: the address serves the same archive however often it is fetched,
    // and a second read of the export announces an address serving those same bytes.
    let again = archive_bytes(&router, case.case, &described).await;
    assert_eq!(
        first, again,
        "the announced address must serve the same archive to every fetch"
    );
    let reread = completed_export(&router, case.case, &export_id).await;
    let from_reread = archive_bytes(&router, case.case, &reread).await;
    assert_eq!(
        first, from_reread,
        "a second read of the export must announce an address for the same archive"
    );

    // An address of the published shape naming an export nothing recorded serves no
    // archive, and neither does the archive's path without the signature that makes it a
    // pre-signed address.
    let url = text(&described, "downloadUrl");
    let (path, query) = url
        .split_once('?')
        .map(|(path, query)| (path.to_string(), query.to_string()))
        .expect("a pre-signed address carries a query");
    let unrecorded = path.replace(&export_id, "Zq7Lm4Xb2P");
    let answered = fetch(&router, &format!("{unrecorded}?{query}")).await;
    assert_eq!(
        answered.status, 404,
        "an address naming an export nothing recorded must serve no archive, got {} {:?}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    let unsigned = fetch(&router, &path).await;
    assert_ne!(
        unsigned.status, 200,
        "the archive's path must serve nothing without the signature that makes it an \
         address this service handed out, got {:?}",
        String::from_utf8_lossy(&unsigned.body)
    );
}

#[tokio::test]
async fn an_archive_is_the_snapshot_of_the_instant_its_job_was_recorded() {
    let router = emulator();
    let fixture = fixture(&router, "ArchiveSnapshotBot").await;
    let cases = cases(&fixture);
    let case = cases
        .iter()
        .find(|case| case.case == "the filled locale of the draft")
        .expect("the archive table holds the filled locale of the draft");

    let before = create_export(&router, case).await;
    let described = completed_export(&router, case.case, &before).await;
    let snapshot = archive_bytes(&router, case.case, &described).await;

    // A resource created AFTER the job was recorded is not in the archive that job
    // assembled, and is in the archive of a job recorded after it.
    const LATER_INTENT: &str = "CancelOrder";
    create_intent(
        &router,
        &fixture.bot_id,
        FILLED_LOCALE,
        &intent_body(LATER_INTENT, json!({})),
    )
    .await;

    let replayed = archive_bytes(&router, case.case, &described).await;
    assert_eq!(
        snapshot, replayed,
        "a change made after the job was recorded must not reach the archive it assembled"
    );
    let later_path = format!(
        "{}/{}/{FILLED_LOCALE}/{}/{LATER_INTENT}/{}",
        fixture.bot_name,
        state::BOT_LOCALES_DIRECTORY,
        state::INTENTS_DIRECTORY,
        state::INTENT_DEFINITION_FILE
    );
    let snapshot_entries = entries(case.case, &snapshot);
    assert!(
        document(&snapshot_entries, &later_path).is_none(),
        "the archive of the earlier job must not hold {later_path}, got {:?}",
        paths_of(&snapshot_entries)
    );

    let after = create_export(&router, case).await;
    let after_described = completed_export(&router, case.case, &after).await;
    let after_bytes = archive_bytes(&router, case.case, &after_described).await;
    let after_entries = entries(case.case, &after_bytes);
    let later = document(&after_entries, &later_path).unwrap_or_else(|| {
        panic!(
            "a job recorded after the change must hold {later_path}, got {:?}",
            paths_of(&after_entries)
        )
    });
    assert_eq!(
        text(&later, "name"),
        LATER_INTENT,
        "the later job's archive must carry the intent created before it, got {later:?}"
    );
}

/// The four bytes a zip archive opens with (the local-file-header signature of the ZIP file
/// format), which is what a client that downloads the announced address reads first.
const ZIP_LOCAL_FILE_HEADER: [u8; 4] = [b'P', b'K', 0x03, 0x04];

/// One locale whose definition files an export's archive must report the stored values of:
/// the locale it is, the confidence threshold it was created with, and the intent created in
/// it with the sample utterances that call was given.
struct Definitions {
    case: &'static str,
    locale_id: &'static str,
    threshold: f64,
    intent_name: &'static str,
    utterances: Vec<&'static str>,
}

/// The table [`an_exported_locale_definition_carries_the_values_it_was_created_with`] runs
/// over.
///
/// The rows differ in the locale, in the confidence threshold — including both bounds the
/// modelled `ConfidenceThreshold` publishes
/// (`smithy_query(query="shape",shape="ConfidenceThreshold")`: a double in range 0..1) — in
/// the intent's name and in how many sample utterances the intent was created with,
/// including none at all, because what a definition file owes is the values the store holds
/// for the resource, not the ones of any one run.
fn definition_cases() -> Vec<Definitions> {
    vec![
        Definitions {
            case: "a threshold between the published bounds, one utterance",
            locale_id: "en_US",
            threshold: 0.4,
            intent_name: "OrderPizza",
            utterances: vec!["I want to order a pizza"],
        },
        Definitions {
            case: "the threshold at the published minimum, two utterances",
            locale_id: "de_DE",
            threshold: 0.0,
            intent_name: "BlumenBestellen",
            utterances: vec!["Ich möchte Blumen bestellen", "Blumen bitte"],
        },
        Definitions {
            case: "the threshold at the published maximum, no utterances",
            locale_id: "ja_JP",
            threshold: 1.0,
            intent_name: "CheckBalance",
            utterances: vec![],
        },
        Definitions {
            case: "a locale id with a numeric region subtag, three utterances",
            locale_id: "es_419",
            threshold: 0.75,
            intent_name: "Transferir-Fondos",
            utterances: vec![
                "Quiero transferir fondos",
                "Transferir dinero",
                "Hacer una transferencia",
            ],
        },
    ]
}

/// The paths an archive holds, for a message that has to say what was there instead.
fn paths_of(entries: &[(String, String)]) -> Vec<String> {
    entries.iter().map(|(path, _)| path.clone()).collect()
}

/// The `sampleUtterances` a definition file carries for an intent created with
/// `utterances` — the member is left out entirely when the intent holds none, which is the
/// same as an absent member on the wire.
fn expected_utterances(utterances: &[&str]) -> Value {
    if utterances.is_empty() {
        return Value::Null;
    }
    Value::Array(
        utterances
            .iter()
            .map(|utterance| json!({ "utterance": utterance }))
            .collect(),
    )
}

#[tokio::test]
async fn an_exported_locale_definition_carries_the_values_it_was_created_with() {
    let router = emulator();
    let bot_name = "ArchiveDefinitionsBot";
    let bot_id = create_bot(&router, &create_body(bot_name, 300, json!({}))).await;
    let cases = definition_cases();

    // Each row gets its OWN locale of the same draft version, created with its own
    // threshold and holding its own intent, so no two rows share a stored value.
    let mut intent_ids: Vec<String> = Vec::new();
    for case in &cases {
        let (status, created) = call(
            &router,
            "PUT",
            &locales_uri(&bot_id, "DRAFT"),
            Some(locale_body(case.locale_id, case.threshold, json!({}))),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {:?}: expected CreateBotLocale to answer 202, got {status} {created:?}",
            case.case
        );
        let utterances = expected_utterances(&case.utterances);
        let extra = if utterances.is_null() {
            json!({})
        } else {
            json!({ "sampleUtterances": utterances })
        };
        intent_ids.push(
            create_intent(
                &router,
                &bot_id,
                case.locale_id,
                &intent_body(case.intent_name, extra),
            )
            .await,
        );
    }

    // Every export is submitted first, so the span the clock owes each job runs alongside
    // the others rather than one after another.
    let mut submitted: Vec<String> = Vec::new();
    for case in &cases {
        let body = json!({
            "resourceSpecification": {
                "botLocaleExportSpecification": {
                    "botId": bot_id,
                    "botVersion": "DRAFT",
                    "localeId": case.locale_id,
                },
            },
            "fileFormat": "LexJson",
        });
        let (status, created) = call(&router, "PUT", EXPORTS_URI, Some(body)).await;
        assert_eq!(
            status, 202,
            "case {:?}: expected CreateExport to answer 202, got {status} {created:?}",
            case.case
        );
        submitted.push(text(&created, "exportId"));
    }

    for ((case, export_id), intent_id) in cases.iter().zip(&submitted).zip(&intent_ids) {
        let name = case.case;

        // A specification that resolved reaches the status in which the export is ready to
        // download, and states no reason for a failure that did not happen.
        let described = completed_export(&router, name, export_id).await;
        assert_eq!(
            member(&described, "failureReasons"),
            Value::Null,
            "case {name:?}: a job whose archive was assembled states no reason for a \
             failure, got {described:?}"
        );

        // The address it announces serves the archive itself.
        let bytes = archive_bytes(&router, name, &described).await;
        assert_eq!(
            bytes.get(..ZIP_LOCAL_FILE_HEADER.len()),
            Some(&ZIP_LOCAL_FILE_HEADER[..]),
            "case {name:?}: the download must open with a zip local-file-header signature, \
             got {:?} of {} bytes",
            bytes.get(..ZIP_LOCAL_FILE_HEADER.len()),
            bytes.len()
        );
        let entries = entries(name, &bytes);

        // The LOCALE's definition, keyed by the bot's name and the locale, carries the
        // locale it is for and the threshold the locale was created with.
        let locale_root = format!(
            "{bot_name}/{}/{}",
            state::BOT_LOCALES_DIRECTORY,
            case.locale_id
        );
        let locale_path = format!("{locale_root}/{}", state::BOT_LOCALE_DEFINITION_FILE);
        let locale = document(&entries, &locale_path).unwrap_or_else(|| {
            panic!(
                "case {name:?}: the archive must hold {locale_path}, got {:?}",
                paths_of(&entries)
            )
        });
        assert_eq!(
            text(&locale, "identifier"),
            case.locale_id,
            "case {name:?}: the locale's definition must name the locale exported, got \
             {locale:?}"
        );
        assert_eq!(
            member(&locale, "nluConfidenceThreshold"),
            json!(case.threshold),
            "case {name:?}: the locale's definition must carry the threshold the locale was \
             created with, got {locale:?}"
        );

        // The INTENT's definition, keyed by the intent's name, carries what CreateIntent
        // was given and the id that call minted.
        let intent_path = format!(
            "{locale_root}/{}/{}/{}",
            state::INTENTS_DIRECTORY,
            case.intent_name,
            state::INTENT_DEFINITION_FILE
        );
        let intent = document(&entries, &intent_path).unwrap_or_else(|| {
            panic!(
                "case {name:?}: the archive must hold {intent_path}, got {:?}",
                paths_of(&entries)
            )
        });
        assert_eq!(
            (text(&intent, "name"), text(&intent, "identifier")),
            (case.intent_name.to_string(), intent_id.clone()),
            "case {name:?}: the intent's definition must carry the name and id it was \
             created under, got {intent:?}"
        );
        assert_eq!(
            member(&intent, "sampleUtterances"),
            expected_utterances(&case.utterances),
            "case {name:?}: the intent's definition must carry exactly the utterances \
             CreateIntent was given, got {intent:?}"
        );
    }
}

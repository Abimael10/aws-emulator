// SPDX-License-Identifier: CC0-1.0

//! The locations Amazon Translate announces for a custom terminology's files,
//! across the two services they span.
//!
//! RULE: a location an operation reports for a CALLER to fetch is an address on
//! the endpoint that caller reached this emulator on, serving the object it
//! names. `GetTerminology` reports `TerminologyDataLocation` ("The Amazon S3
//! location of the most recent custom terminology input file that was
//! successfully imported into Amazon Translate. The location is returned as a
//! presigned URL that has a 30-minute expiration") and both it and
//! `ImportTerminology` report `AuxiliaryDataLocation` ("The Amazon S3 location of
//! a file that provides any errors or warnings that were produced by your input
//! file"), so the documented workflow is to read the resource and then download
//! the file from the value returned. This emulator is reachable at its own
//! endpoint alone and no production AWS hostname resolves to it, so an address on
//! such a hostname can never be followed from the environment the API was called
//! in — the member would be a dead link.
//!
//! Only a cross-service suite can pin this: the terminology is Amazon Translate's
//! and the object behind its announced location is the object store's, so what
//! the location serves is observable only where both are mounted on one engine
//! and the announced URL is really fetched.
//!
//! The rows differ along what the rule generalises over: the format the file was
//! imported in (each of the three the model declares), the directionality of the
//! resource (which decides the formats a read may be asked for), whether the
//! input file held rows the service skipped (which decides whether there is an
//! auxiliary file at all), and the format a read asks the file back in.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

/// The `X-Amz-Target` prefix Amazon Translate's operations are named under.
const TARGET_PREFIX: &str = "AWSShineFrontendService_20170701";

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The base URL this emulator is reached on in these cases. A value of its own,
/// so an announced address is pinned to the endpoint the request arrived on
/// rather than to whatever a default happens to be.
const URL_BASE: &str = "https://emulator.test:9443";

/// The bucket this account's terminology files live in, as the Translate crate
/// names it.
fn bucket() -> String {
    format!("translate-terminology-{}-{REGION}", engine::EMULATOR_ACCOUNT_ID)
}

/// One emulator carrying both services over ONE shared engine, reached at
/// [`URL_BASE`], exactly as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are verified by the emulator, never by a service; these cases
    // are about the address announced and what it serves, so they sign with a
    // fixed scope and verify nothing.
    router.set_request_authentication(false);
    router.set_url_base(EndpointUrl::parse(URL_BASE).expect("a usable endpoint"));
    router.register_service(
        service_com_amazonaws_translate_awsshinefrontendservice_20170701::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One awsJson1_1 call to Amazon Translate, answered with the status and body.
async fn translate(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("translate.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/translate/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A translate call that must succeed.
async fn translate_ok(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    case: &str,
) -> Value {
    let (status, parsed) = translate(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} {parsed}"
    );
    parsed
}

/// Fetch an announced location the way a client that was handed it does: a `GET`
/// of exactly that URL, carrying no credentials of its own — the signature the URL
/// carries is the authority the fetch presents.
async fn fetch(router: &MultiServiceRouter, url: &str) -> (u16, Bytes) {
    let authority = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .expect("an announced location names a host");
    let request = http::Request::builder()
        .method("GET")
        .uri(url)
        .header("host", authority)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.into_body())
}

/// The member of a response that carries a location.
fn string_at(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One import request body: the file, the format it declares and the
/// directionality it declares, under `name`.
fn import_body(
    format: &str,
    directionality: Option<&str>,
    name: &str,
    file: &[u8],
) -> Value {
    let mut data = json!({
        "File": aws_smithy_types::base64::encode(file),
        "Format": format,
    });
    if let Some(directionality) = directionality {
        data["Directionality"] = json!(directionality);
    }
    json!({ "Name": name, "MergeStrategy": "OVERWRITE", "TerminologyData": data })
}

/// The three formats a terminology file is imported and retrieved in
/// (`smithy_query shape=TerminologyDataFormat`).
const FORMATS: [&str; 3] = ["CSV", "TSV", "TMX"];

/// One row of the table: a terminology file, the format it is imported in, the
/// directionality it declares, and what its content amounts to.
struct LocationCase {
    name: &'static str,
    file: &'static [u8],
    format: &'static str,
    directionality: Option<&'static str>,
    /// The languages the file declares, source first.
    languages: &'static [&'static str],
    /// How many of its rows state a term.
    terms: i64,
    /// How many of its rows the service cannot use, which is what decides
    /// whether the resource has an auxiliary file at all.
    skipped: i64,
}

impl LocationCase {
    /// Whether a read of this resource may be asked for a format other than the
    /// one it was imported in: a multi-directional resource may not.
    fn multi_directional(&self) -> bool {
        self.directionality == Some("MULTI")
    }

    /// This row's own import request body.
    fn import(&self) -> Value {
        import_body(self.format, self.directionality, self.name, self.file)
    }
}

/// The rows: each format, each directionality, and a file with warnings to
/// report beside the files that have none.
const CASES: &[LocationCase] = &[
    LocationCase {
        name: "csv-terms",
        file: b"en,fr\nhello,bonjour\nworld,monde\n",
        format: "CSV",
        directionality: None,
        languages: &["en", "fr"],
        terms: 2,
        skipped: 0,
    },
    LocationCase {
        name: "tsv-terms",
        file: b"en\tde\nhello\thallo\n",
        format: "TSV",
        directionality: Some("UNI"),
        languages: &["en", "de"],
        terms: 1,
        skipped: 0,
    },
    LocationCase {
        name: "tmx-terms",
        file: br#"<?xml version="1.0" encoding="UTF-8"?>
<tmx version="1.4">
  <header srclang="en"/>
  <body>
    <tu><tuv xml:lang="en"><seg>hello</seg></tuv><tuv xml:lang="es"><seg>hola</seg></tuv></tu>
  </body>
</tmx>
"#,
        format: "TMX",
        directionality: None,
        languages: &["en", "es"],
        terms: 1,
        skipped: 0,
    },
    LocationCase {
        name: "multi-terms",
        file: b"en,fr,de\nhello,bonjour,hallo\n",
        format: "CSV",
        directionality: Some("MULTI"),
        languages: &["en", "fr", "de"],
        terms: 1,
        skipped: 0,
    },
    LocationCase {
        name: "partial-terms",
        file: b"en,it\nhello,ciao\n,\nonly-source,\n",
        format: "CSV",
        directionality: None,
        languages: &["en", "it"],
        terms: 1,
        skipped: 2,
    },
];

/// Every location a read announces is an address on the endpoint the request
/// reached this emulator on, and a fetch of it serves the file the import stored:
/// the uploaded bytes themselves in the format the file was imported in.
#[tokio::test]
async fn an_announced_terminology_location_serves_the_imported_file() {
    let router = emulator();
    for case in CASES {
        let name = case.name;
        let imported = translate_ok(&router, "ImportTerminology", &case.import(), name).await;
        assert_eq!(
            imported.pointer("/TerminologyProperties/TermCount"),
            Some(&json!(case.terms)),
            "case {name:?}: expected {} term(s), got {imported}",
            case.terms
        );

        let read = translate_ok(&router, "GetTerminology", &json!({"Name": name}), name).await;
        let location = string_at(&read, "/TerminologyDataLocation/Location");
        assert_eq!(
            string_at(&read, "/TerminologyDataLocation/RepositoryType"),
            "S3",
            "case {name:?}: the location names its repository, got {read}"
        );
        let expected_prefix = format!(
            "{URL_BASE}/{}/{name}/{name}.{}?",
            bucket(),
            case.format.to_ascii_lowercase()
        );
        assert!(
            location.starts_with(&expected_prefix),
            "case {name:?}: expected a location at {expected_prefix:?}, got {location:?}"
        );
        assert!(
            !location.contains("amazonaws.com"),
            "case {name:?}: a location must name no host this emulator does not serve, \
             got {location:?}"
        );
        for field in ["X-Amz-Algorithm=AWS4-HMAC-SHA256", "X-Amz-Expires=1800", "X-Amz-Signature="]
        {
            assert!(
                location.contains(field),
                "case {name:?}: expected a presigned location carrying {field:?}, \
                 got {location:?}"
            );
        }

        let (status, body) = fetch(&router, &location).await;
        assert_eq!(
            status, 200,
            "case {name:?}: a fetch of {location:?} answered {status} {}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(
            body.as_ref(),
            case.file,
            "case {name:?}: the location must serve the imported file, got {:?}",
            String::from_utf8_lossy(&body)
        );
    }
}

/// A read asked for another format announces a location serving that same
/// terminology rendered into it — so every format a read may ask for names an
/// object that is there. A multi-directional resource is refused another format,
/// so it has none of these.
#[tokio::test]
async fn a_location_in_another_format_serves_that_terminology_rendered_into_it() {
    let router = emulator();
    for case in CASES {
        let name = case.name;
        translate_ok(&router, "ImportTerminology", &case.import(), name).await;

        for format in FORMATS {
            if format == case.format {
                continue;
            }
            let request = json!({"Name": name, "TerminologyDataFormat": format});
            let (status, read) = translate(&router, "GetTerminology", &request).await;
            if case.multi_directional() {
                assert_eq!(
                    status, 400,
                    "case {name:?} in {format}: a multi-directional resource is only \
                     retrievable in its own format, got {status} {read}"
                );
                continue;
            }
            assert_eq!(
                status, 200,
                "case {name:?} in {format}: expected 200, got {status} {read}"
            );
            let location = string_at(&read, "/TerminologyDataLocation/Location");
            assert!(
                location.starts_with(&format!(
                    "{URL_BASE}/{}/{name}/{name}.{}?",
                    bucket(),
                    format.to_ascii_lowercase()
                )),
                "case {name:?} in {format}: expected a location of this emulator naming that \
                 format, got {location:?}"
            );
            let (status, body) = fetch(&router, &location).await;
            assert_eq!(
                status, 200,
                "case {name:?} in {format}: a fetch of {location:?} answered {status} {}",
                String::from_utf8_lossy(&body)
            );

            // What it serves is a terminology file of that format holding this
            // resource's terms: importing it back states the same languages and
            // the same number of terms.
            let round_trip = format!("{name}-as-{}", format.to_ascii_lowercase());
            let reimported = translate_ok(
                &router,
                "ImportTerminology",
                &import_body(format, case.directionality, &round_trip, &body),
                &round_trip,
            )
            .await;
            assert_eq!(
                reimported.pointer("/TerminologyProperties/TermCount"),
                Some(&json!(case.terms)),
                "case {name:?} in {format}: the rendered file must hold the same terms, \
                 got {reimported}"
            );
            assert_eq!(
                string_at(&reimported, "/TerminologyProperties/SourceLanguageCode"),
                case.languages[0],
                "case {name:?} in {format}: the rendered file must declare the same source \
                 language, got {reimported}"
            );
            assert_eq!(
                reimported.pointer("/TerminologyProperties/TargetLanguageCodes"),
                Some(&json!(case.languages[1..])),
                "case {name:?} in {format}: the rendered file must declare the same target \
                 languages, got {reimported}"
            );
        }
    }
}

/// The auxiliary location exists exactly for an import that had warnings to
/// report, and it serves those warnings — one row per warning under a header row.
#[tokio::test]
async fn the_auxiliary_location_serves_the_warnings_the_import_produced() {
    let router = emulator();
    for case in CASES {
        let name = case.name;
        let imported = translate_ok(&router, "ImportTerminology", &case.import(), name).await;
        let read = translate_ok(&router, "GetTerminology", &json!({"Name": name}), name).await;

        if case.skipped == 0 {
            for (operation, body) in [("ImportTerminology", &imported), ("GetTerminology", &read)] {
                assert!(
                    body.pointer("/AuxiliaryDataLocation").is_none(),
                    "case {name:?}: an import that skipped nothing has no auxiliary file, \
                     got {operation} {body}"
                );
            }
            continue;
        }

        for (operation, body) in [("ImportTerminology", &imported), ("GetTerminology", &read)] {
            let location = string_at(body, "/AuxiliaryDataLocation/Location");
            assert!(
                location.starts_with(&format!(
                    "{URL_BASE}/{}/{name}/{name}-auxiliary.csv?",
                    bucket()
                )),
                "case {name:?}: {operation} must announce the auxiliary file on this \
                 emulator, got {location:?}"
            );
            let (status, served) = fetch(&router, &location).await;
            assert_eq!(
                status, 200,
                "case {name:?}: a fetch of {operation}'s {location:?} answered {status} {}",
                String::from_utf8_lossy(&served)
            );
            let text = String::from_utf8_lossy(&served).to_string();
            let rows: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
            assert_eq!(
                rows.first().copied(),
                Some("Number,Warning"),
                "case {name:?}: the auxiliary file's first row names its columns, got {text:?}"
            );
            assert_eq!(
                i64::try_from(rows.len() - 1).unwrap_or_default(),
                case.skipped,
                "case {name:?}: expected one row per skipped row of the input file, \
                 got {text:?}"
            );
        }
    }
}

/// A deleted terminology serves nothing where its locations pointed: the resource
/// is gone, so the file behind the address it announced is withdrawn with it.
#[tokio::test]
async fn deleting_a_terminology_withdraws_the_file_its_location_named() {
    let router = emulator();
    for case in CASES {
        let name = case.name;
        translate_ok(&router, "ImportTerminology", &case.import(), name).await;
        let read = translate_ok(&router, "GetTerminology", &json!({"Name": name}), name).await;
        let location = string_at(&read, "/TerminologyDataLocation/Location");
        let (status, _) = fetch(&router, &location).await;
        assert_eq!(
            status, 200,
            "case {name:?}: the location must serve the file before the delete"
        );

        translate_ok(&router, "DeleteTerminology", &json!({"Name": name}), name).await;
        let (status, body) = fetch(&router, &location).await;
        assert_eq!(
            status, 404,
            "case {name:?}: a withdrawn file must answer 404, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }
}

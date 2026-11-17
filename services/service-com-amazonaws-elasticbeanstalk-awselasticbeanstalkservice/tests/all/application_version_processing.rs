// SPDX-License-Identifier: CC0-1.0

//! `CreateApplicationVersion(Process=true)` PRE-PROCESSES the source bundle it
//! names, so the application version reaches a terminal
//! `ApplicationVersionStatus` instead of sitting at `Unprocessed` forever.
//!
//! The rule under test: a create that asks for pre-processing validates "the
//! environment manifest (`env.yaml`) and configuration files (`*.config` files in
//! the `.ebextensions` folder) in the source bundle" and the version reports
//! `Processing` ("Elastic Beanstalk is currently processing the application
//! version") until that work is due, then the status it reached — `Processed`
//! ("was successfully pre-processed and validated") or `Failed` ("configuration
//! files didn't pass validation") — while a create that does NOT ask for it
//! leaves the version `Unprocessed` ("Application version wasn't pre-processed or
//! validated").
//!
//! The verdict is reached by READING the archive, so the table below spans what
//! the rule generalises over: which of the two kinds of file the bundle carries,
//! whether it carries either at all, the syntax a configuration file is written
//! in (YAML, and the JSON that YAML is a superset of), where in the archive the
//! file sits, whether the file is text, and whether the bundle is an archive at
//! all. Every suite here mounts the S3 service beside Elastic Beanstalk on ONE
//! engine and uploads the bundle through the S3 front door, so the archive the
//! create pre-processes is one object storage really holds.

use crate::harness::{instant, wait_for_version_status, Answer, Emulator, PAST_DUE};
use crate::lifecycle::TEST_REGION;

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The bucket the suites upload a source bundle into, and the key under it.
const BUNDLE_BUCKET: &str = "eb-source-bundles";
/// A nested key, the shape a client's build pipeline uploads under.
const BUNDLE_KEY: &str = "bundles/2026/app.zip";

/// What a source bundle IS: an archive carrying these entries, or bytes that are
/// no archive at all.
///
/// `pub(crate)`: the platform-build suite uploads the OTHER caller-named archive
/// (`CreatePlatformVersion$PlatformDefinitionBundle`) and writes it the same way
/// a client does, so neither suite keeps a second archive writer.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Bundle {
    /// A `.zip` whose entries are `(path, content)`.
    Zip(&'static [(&'static str, &'static [u8])]),
    /// Bytes that no archive reader will read.
    Raw(&'static [u8]),
}

impl Bundle {
    /// The bytes a client uploads for this bundle.
    pub(crate) fn bytes(self) -> Vec<u8> {
        match self {
            Bundle::Raw(bytes) => bytes.to_vec(),
            Bundle::Zip(entries) => zip_of(entries),
        }
    }
}

/// The bytes of a `.zip` carrying `entries` as `(path, content)` — what a client
/// uploads a source bundle as.
///
/// `pub(crate)` and taking a borrowed slice: a suite whose entries are built at
/// RUN time (a manifest naming a solution stack read off the service's own
/// catalogue) writes its archive with this one writer too, so no suite of this
/// target keeps a second archive writer.
pub(crate) fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        for (path, content) in entries {
            let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
            writer
                .start_file(*path, options)
                .expect("the archive entry starts");
            std::io::Write::write_all(&mut writer, content).expect("the archive entry's bytes");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// One source bundle a create asks to have pre-processed, and the terminal
/// `ApplicationVersionStatus` the version must settle in for it.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    bundle: Bundle,
    /// The modelled status, taken from the service's own constants so the
    /// expectation cannot drift from what the service reports.
    terminal: &'static str,
}

/// A valid environment manifest, in the form `env.yaml` documents: a mapping of
/// manifest keys.
const VALID_MANIFEST: &[u8] =
    b"AWSConfigurationTemplateVersion: 1.1.0.0\nEnvironmentName: demo-env\n";

/// A configuration file written as YAML.
const YAML_CONFIG: &[u8] =
    b"option_settings:\n  aws:elasticbeanstalk:application:environment:\n    PORT: '8080'\n";

/// The same kind of configuration file written as JSON, which a `.config` file
/// may be: YAML 1.2 is a superset of JSON, so the same reader takes both.
const JSON_CONFIG: &[u8] =
    br#"{"option_settings": {"aws:elasticbeanstalk:xray": {"XRayEnabled": "true"}}}"#;

/// Text that is not a YAML document: a flow sequence that is never closed.
const BROKEN_YAML: &[u8] = b"option_settings: [\n";

/// Bytes that are not text at all, so no manifest can be parsed out of them.
const NOT_TEXT: &[u8] = b"\x00\xff\xfe\x01manifest";

/// One row per thing the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "a bundle carrying neither a manifest nor a configuration file",
        bundle: Bundle::Zip(&[
            ("Procfile", b"web: python3 server.py"),
            ("server.py", b"print('ok')\n"),
        ]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        case: "an archive with no entries at all",
        bundle: Bundle::Zip(&[]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        case: "a bundle whose env.yaml is a YAML document",
        bundle: Bundle::Zip(&[("env.yaml", VALID_MANIFEST), ("Procfile", b"web: true")]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        case: "a bundle whose .ebextensions configuration file is YAML",
        bundle: Bundle::Zip(&[(".ebextensions/options.config", YAML_CONFIG)]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        case: "a bundle whose .ebextensions configuration file is JSON",
        bundle: Bundle::Zip(&[(".ebextensions/01-xray.config", JSON_CONFIG)]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        case: "a bundle carrying a manifest and several configuration files",
        bundle: Bundle::Zip(&[
            ("env.yaml", VALID_MANIFEST),
            (".ebextensions/01-xray.config", JSON_CONFIG),
            (".ebextensions/02-options.config", YAML_CONFIG),
        ]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        // "*.config files IN the .ebextensions folder": a file deeper under it is
        // not one of them, so its syntax is not what the version is judged on.
        case: "a bundle whose broken config sits deeper than the .ebextensions folder",
        bundle: Bundle::Zip(&[
            (".ebextensions/nested/broken.config", BROKEN_YAML),
            ("Procfile", b"web: true"),
        ]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        case: "a bundle whose .ebextensions file is not a configuration file",
        bundle: Bundle::Zip(&[(".ebextensions/README.md", BROKEN_YAML)]),
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Row {
        case: "a bundle whose env.yaml is not a YAML document",
        bundle: Bundle::Zip(&[("env.yaml", BROKEN_YAML), ("Procfile", b"web: true")]),
        terminal: eb::state::VERSION_STATUS_FAILED,
    },
    Row {
        case: "a bundle whose .ebextensions configuration file is not a YAML document",
        bundle: Bundle::Zip(&[
            (".ebextensions/01-xray.config", JSON_CONFIG),
            (".ebextensions/02-broken.config", BROKEN_YAML),
        ]),
        terminal: eb::state::VERSION_STATUS_FAILED,
    },
    Row {
        case: "a bundle whose env.yaml is not text",
        bundle: Bundle::Zip(&[("env.yaml", NOT_TEXT)]),
        terminal: eb::state::VERSION_STATUS_FAILED,
    },
    Row {
        case: "a bundle that is not an archive",
        bundle: Bundle::Raw(b"this is not an archive, it is a sentence"),
        terminal: eb::state::VERSION_STATUS_FAILED,
    },
];

/// One file of a source bundle that is past the bounds this emulator parses
/// caller content within: which of the two pre-processed files it is, and the
/// text it carries.
struct DeepFile<'a> {
    case: &'a str,
    path: &'a str,
    text: &'a [u8],
}

/// A file of the bundle that is past those bounds leaves the version in the
/// status nothing validated — and leaves the emulator serving.
///
/// Both bounds are taken BEFORE a tree is built from the text, because by then
/// the work is already done: a YAML parser costs a level of recursion per
/// nested collection (and a level costs as little as one byte), and a loader
/// resolves an ALIAS by copying the anchored node, so a small shallow document
/// can expand into orders of magnitude more nodes. A file past either bound is
/// never loaded, so there is no verdict about it (`Unprocessed`, "Application
/// version wasn't pre-processed or validated") rather than a validation failure
/// the reading never reached. The rows differ along WHICH of the two
/// pre-processed files carries it and along the form it takes: nesting in flow
/// collections, nesting in block sequence entries, and alias amplification.
#[tokio::test]
async fn a_bundle_file_past_the_parse_bounds_leaves_the_version_unvalidated() {
    let unprocessed = eb::state::version_status_on_the_wire(eb::state::VERSION_STATUS_UNPROCESSED);
    // One level past the bound in each form the shared pre-parse scan counts:
    // flow collections, and block sequence entries.
    let flow = format!(
        "option_settings: {}",
        "[".repeat(eb::state::MAX_VALIDATED_NESTING + 1)
    );
    let block: String = (0..=eb::state::MAX_VALIDATED_NESTING)
        .map(|level| format!("{}- x\n", " ".repeat(level)))
        .collect();
    // A kilobyte of ALIAS amplification: each level names the one below it twice,
    // so a loader that expanded the aliases would build a tree doubling with
    // every level while the text itself stays small and shallow.
    let mut aliases = String::from("a0: &a0 [x, x]\n");
    for level in 1..24 {
        aliases.push_str(&format!(
            "a{level}: &a{level} [*a{}, *a{}]\n",
            level - 1,
            level - 1
        ));
    }
    let rows: [DeepFile<'_>; 5] = [
        DeepFile {
            case: "an env.yaml of nested flow collections",
            path: "env.yaml",
            text: flow.as_bytes(),
        },
        DeepFile {
            case: "a configuration file of nested flow collections",
            path: ".ebextensions/deep.config",
            text: flow.as_bytes(),
        },
        DeepFile {
            case: "a configuration file of nested block sequence entries",
            path: ".ebextensions/deep.config",
            text: block.as_bytes(),
        },
        DeepFile {
            case: "an env.yaml whose aliases would expand exponentially",
            path: "env.yaml",
            text: aliases.as_bytes(),
        },
        DeepFile {
            case: "a configuration file whose aliases would expand exponentially",
            path: ".ebextensions/aliases.config",
            text: aliases.as_bytes(),
        },
    ];
    for DeepFile { case, path, text } in rows {
        let files = [(path, text)];
        let emulator = Emulator::with_object_storage();
        emulator
            .put_object(BUNDLE_BUCKET, BUNDLE_KEY, &zip_of(&files))
            .await;
        given_application(&emulator, "deep-app").await;

        let created = emulator
            .call(
                "CreateApplicationVersion",
                &[
                    ("ApplicationName", "deep-app"),
                    ("VersionLabel", "v1"),
                    ("SourceBundle.S3Bucket", BUNDLE_BUCKET),
                    ("SourceBundle.S3Key", BUNDLE_KEY),
                    ("Process", "true"),
                ],
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateApplicationVersion, got {} and body {}",
            created.status, created.body
        );
        assert_eq!(
            created.value("ApplicationVersion/Status"),
            Some(unprocessed.as_str()),
            "case {case:?}: a file nothing parsed leaves the version {unprocessed}, got body {}",
            created.body
        );
        // And the emulator is still serving: the next call is answered from the
        // same process.
        let (settled, answer) = described_status(&emulator, "deep-app", "v1").await;
        assert_eq!(
            settled, unprocessed,
            "case {case:?}: expected {unprocessed} from the reader too, got {settled:?} and body \
             {}",
            answer.body
        );
    }
}

/// The status `DescribeApplicationVersions` reports for `label` under
/// `application`, with the answer it was read from for a failing assertion.
async fn described_status(emulator: &Emulator, application: &str, label: &str) -> (String, Answer) {
    let described = emulator
        .call(
            "DescribeApplicationVersions",
            &[
                ("ApplicationName", application),
                ("VersionLabels.member.1", label),
            ],
        )
        .await;
    let status = described
        .value("ApplicationVersions/member/Status")
        .unwrap_or_default()
        .to_string();
    (status, described)
}

/// Create the application a version is created under.
async fn given_application(emulator: &Emulator, name: &str) {
    let created = emulator
        .call("CreateApplication", &[("ApplicationName", name)])
        .await;
    assert_eq!(
        created.status, 200,
        "seeding application {name:?}: expected 200 from CreateApplication, got {} and body {}",
        created.status, created.body
    );
}

/// Every bundle in [`ROWS`] is really read: the version reports `Processing`
/// straight after the create and then the status that reading reached, and a read
/// does not move it on by itself.
#[tokio::test]
async fn pre_processing_settles_a_version_in_the_status_its_bundle_validated_to() {
    let processing = eb::state::version_status_on_the_wire(eb::state::VERSION_STATUS_PROCESSING);
    for row in ROWS {
        let case = row.case;
        let emulator = Emulator::with_object_storage();
        emulator
            .put_object(BUNDLE_BUCKET, BUNDLE_KEY, &row.bundle.bytes())
            .await;
        given_application(&emulator, "processing-app").await;

        let created = emulator
            .call(
                "CreateApplicationVersion",
                &[
                    ("ApplicationName", "processing-app"),
                    ("VersionLabel", "v1"),
                    ("SourceBundle.S3Bucket", BUNDLE_BUCKET),
                    ("SourceBundle.S3Key", BUNDLE_KEY),
                    ("Process", "true"),
                ],
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateApplicationVersion, got {} and body {}",
            created.status, created.body
        );
        // The create answers with the transient status: the work it started is
        // not done when the call returns.
        assert_eq!(
            created.value("ApplicationVersion/Status"),
            Some(processing.as_str()),
            "case {case:?}: expected the create to report {processing}, got body {}",
            created.body
        );
        // A caller polling straight after the create still sees it.
        let (immediately, answer) = described_status(&emulator, "processing-app", "v1").await;
        assert_eq!(
            immediately, processing,
            "case {case:?}: expected {processing} immediately after the create, got \
             {immediately:?} and body {}",
            answer.body
        );

        std::thread::sleep(PAST_DUE);
        let expected = eb::state::version_status_on_the_wire(row.terminal);
        let (settled, answer) = described_status(&emulator, "processing-app", "v1").await;
        assert_eq!(
            settled, expected,
            "case {case:?}: expected the version to settle in {expected}, got {settled:?} and \
             body {}",
            answer.body
        );
        // A terminal status is terminal: reading it again reports the same thing.
        let (again, answer) = described_status(&emulator, "processing-app", "v1").await;
        assert_eq!(
            again, expected,
            "case {case:?}: a read must not change what a later read sees, got {again:?} and \
             body {}",
            answer.body
        );
    }
}

/// A create that does NOT ask for pre-processing leaves the version
/// `Unprocessed`: "processing is optional" for a source bundle in Amazon S3, and
/// a version nothing validated is not reported as validated — not even when its
/// bundle would not have passed validation.
#[tokio::test]
async fn a_version_whose_create_asks_for_no_processing_stays_unprocessed() {
    let unprocessed = eb::state::version_status_on_the_wire(eb::state::VERSION_STATUS_UNPROCESSED);
    // The same unprocessable bundle in every row, so what differs is only what
    // the request said about `Process`.
    let bundle = Bundle::Zip(&[("env.yaml", BROKEN_YAML)]).bytes();
    for (case, process) in [
        ("Process absent", None),
        ("Process=false", Some("false")),
    ] {
        let emulator = Emulator::with_object_storage();
        emulator.put_object(BUNDLE_BUCKET, BUNDLE_KEY, &bundle).await;
        given_application(&emulator, "unprocessed-app").await;

        let mut params = vec![
            ("ApplicationName", "unprocessed-app"),
            ("VersionLabel", "v1"),
            ("SourceBundle.S3Bucket", BUNDLE_BUCKET),
            ("SourceBundle.S3Key", BUNDLE_KEY),
        ];
        if let Some(process) = process {
            params.push(("Process", process));
        }
        let created = emulator.call("CreateApplicationVersion", &params).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateApplicationVersion, got {} and body {}",
            created.status, created.body
        );
        assert_eq!(
            created.value("ApplicationVersion/Status"),
            Some(unprocessed.as_str()),
            "case {case:?}: expected the create to report {unprocessed}, got body {}",
            created.body
        );

        std::thread::sleep(PAST_DUE);
        let (settled, answer) = described_status(&emulator, "unprocessed-app", "v1").await;
        assert_eq!(
            settled, unprocessed,
            "case {case:?}: a version nothing pre-processed stays {unprocessed}, got {settled:?} \
             and body {}",
            answer.body
        );
    }
}

/// The flow a client that wants to deploy follows end to end: ask for the
/// account's storage location, upload a source bundle into it, create a version
/// with `Process=true` and poll until the version is deployable.
///
/// The settled status is reported by every reader of the record — the create's
/// own answer, `DescribeApplicationVersions` and `UpdateApplicationVersion` — and
/// the version's `DateUpdated` records when the status CHANGED, so it is later
/// than its `DateCreated`.
#[tokio::test]
async fn a_bundle_uploaded_to_the_storage_location_processes_to_a_deployable_version() {
    let case = "the storage location flow";
    let emulator = Emulator::with_object_storage();
    let location = emulator.call("CreateStorageLocation", &[]).await;
    assert_eq!(
        location.status, 200,
        "case {case:?}: expected 200 from CreateStorageLocation, got {} and body {}",
        location.status, location.body
    );
    let bucket = location
        .value("S3Bucket")
        .expect("CreateStorageLocation reports the account's bucket")
        .to_string();
    assert_eq!(
        bucket,
        eb::state::storage_bucket_name(TEST_REGION),
        "case {case:?}: the storage location must be the account's own bucket, got body {}",
        location.body
    );

    let bundle = Bundle::Zip(&[
        ("env.yaml", VALID_MANIFEST),
        (".ebextensions/01-options.config", YAML_CONFIG),
        ("Procfile", b"web: python3 server.py"),
        ("server.py", b"print('ok')\n"),
    ])
    .bytes();
    let key = "sim2real-eb/deployable/v1.zip";
    // The storage location's bucket already exists — `CreateStorageLocation`
    // stood it up — so the upload is the PutObject alone, over the S3 front door.
    let stored = emulator
        .s3("PUT", &format!("/{bucket}/{key}"), &bundle)
        .await;
    assert_eq!(
        stored, 200,
        "case {case:?}: expected 200 from PutObject into the storage location, got {stored}"
    );
    given_application(&emulator, "deployable").await;

    let created = emulator
        .call(
            "CreateApplicationVersion",
            &[
                ("ApplicationName", "deployable"),
                ("VersionLabel", "v1"),
                ("SourceBundle.S3Bucket", &bucket),
                ("SourceBundle.S3Key", key),
                ("Process", "true"),
            ],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplicationVersion, got {} and body {}",
        created.status, created.body
    );
    let date_created = instant(
        created.expect("ApplicationVersion/DateCreated", case),
        case,
    );

    std::thread::sleep(PAST_DUE);
    let processed = eb::state::version_status_on_the_wire(eb::state::VERSION_STATUS_PROCESSED);
    let (settled, answer) = described_status(&emulator, "deployable", "v1").await;
    assert_eq!(
        settled, processed,
        "case {case:?}: a valid bundle must process cleanly, got {settled:?} and body {}",
        answer.body
    );
    let date_updated = instant(
        answer.expect("ApplicationVersions/member/DateUpdated", case),
        case,
    );
    assert!(
        date_updated > date_created,
        "case {case:?}: the status change must be dated after the create, got DateUpdated \
         {date_updated} and DateCreated {date_created}"
    );

    // The same record read through the other reader of an application version.
    let updated = emulator
        .call(
            "UpdateApplicationVersion",
            &[
                ("ApplicationName", "deployable"),
                ("VersionLabel", "v1"),
                ("Description", "ready to deploy"),
            ],
        )
        .await;
    assert_eq!(
        updated.value("ApplicationVersion/Status"),
        Some(processed.as_str()),
        "case {case:?}: UpdateApplicationVersion must report the settled status, got body {}",
        updated.body
    );

    // And the processed version is what an environment is launched from.
    let launched = emulator
        .call(
            "CreateEnvironment",
            &[
                ("ApplicationName", "deployable"),
                ("EnvironmentName", "deployable-env"),
                ("VersionLabel", "v1"),
                ("SolutionStackName", crate::harness::SOLUTION_STACK.as_str()),
            ],
        )
        .await;
    assert_eq!(
        launched.status, 200,
        "case {case:?}: expected 200 from CreateEnvironment, got {} and body {}",
        launched.status, launched.body
    );
    assert_eq!(
        launched.value("VersionLabel"),
        Some("v1"),
        "case {case:?}: the environment must run the processed version, got body {}",
        launched.body
    );
}

// ---------------------------------------------------------------------------
// Every version a client publishes settles on its own
// ---------------------------------------------------------------------------

/// One application version a client publishes: the bundle it is created from,
/// whether its create asks for that bundle to be pre-processed, and the terminal
/// `ApplicationVersionStatus` the version must settle in.
#[derive(Debug, Clone, Copy)]
struct Published {
    case: &'static str,
    /// The application the version belongs to, so the same label under two
    /// applications is two different versions.
    application: &'static str,
    label: &'static str,
    bundle: Bundle,
    process: bool,
    terminal: &'static str,
}

/// The application most of the fleet is published under.
const FLEET_APPLICATION: &str = "fleet-app";
/// A second application on the same engine, carrying a label the first one
/// carries too.
const OTHER_APPLICATION: &str = "other-fleet-app";

/// The versions one client publishes, differing along what the rule
/// generalises over: how MANY versions are outstanding at once, which verdict
/// each one's own bundle reaches (so the three terminal statuses are all
/// outstanding together), whether its create asked for pre-processing at all,
/// and which application the label belongs to. Which bundle SHAPES reach which
/// verdict is the table at the top of this file; these rows carry one bundle per
/// verdict.
const PUBLISHED: &[Published] = &[
    Published {
        case: "a manifest and a configuration file",
        application: FLEET_APPLICATION,
        label: "v-manifest",
        bundle: Bundle::Zip(&[
            ("env.yaml", VALID_MANIFEST),
            (".ebextensions/01-options.config", YAML_CONFIG),
        ]),
        process: true,
        terminal: eb::state::VERSION_STATUS_PROCESSED,
    },
    Published {
        case: "a manifest that is not a YAML document",
        application: FLEET_APPLICATION,
        label: "v-broken-manifest",
        bundle: Bundle::Zip(&[("env.yaml", BROKEN_YAML), ("Procfile", b"web: true")]),
        process: true,
        terminal: eb::state::VERSION_STATUS_FAILED,
    },
    Published {
        // "Many of the values are only applicable if you specified True for the
        // Process parameter": a create that asks for nothing to be pre-processed
        // leaves its version `Unprocessed` however good its bundle is, and that
        // is terminal too. That it is still `Unprocessed` a whole transition span
        // later — rather than merely at the create — is what
        // `a_version_whose_create_asks_for_no_processing_stays_unprocessed`
        // above asserts; this row is here so one application carries a version in
        // each terminal status at once.
        case: "a valid bundle whose create asked for no pre-processing",
        application: FLEET_APPLICATION,
        label: "v-no-process",
        bundle: Bundle::Zip(&[("env.yaml", VALID_MANIFEST)]),
        process: false,
        terminal: eb::state::VERSION_STATUS_UNPROCESSED,
    },
    Published {
        // The same label as the first row, under another application and
        // carrying another bundle: the verdict belongs to the version, not to
        // the label.
        case: "the same label under another application",
        application: OTHER_APPLICATION,
        label: "v-manifest",
        bundle: Bundle::Zip(&[("env.yaml", NOT_TEXT)]),
        process: true,
        terminal: eb::state::VERSION_STATUS_FAILED,
    },
];

/// The object each published version's bundle is uploaded as: one object per
/// version, so no two versions are judged on the same archive.
fn bundle_key(row: &Published) -> String {
    format!("bundles/{}/{}.zip", row.application, row.label)
}

/// The status `described` reports for `label`, read out of the member that
/// carries that label rather than by position — so a report that paired the
/// statuses with the wrong versions could not pass.
fn reported_status(described: &Answer, label: &str) -> Option<String> {
    described
        .elements_at("ApplicationVersions/member")
        .into_iter()
        .find(|member| member.value("VersionLabel") == Some(label))
        .and_then(|member| member.value("Status").map(str::to_string))
}

/// A client publishes SEVERAL versions and every one of them settles in the
/// terminal status its OWN bundle validated to, so each becomes deployable (or
/// reports why it is not) on its own.
///
/// The rule the rows span: pre-processing is per application version — the
/// archive THAT create named is read and the verdict that reading reached is the
/// status THAT version settles in one transition span later — so a fleet of
/// outstanding versions settles version by version, and a label another
/// application carries is a different version with its own verdict. Every status
/// is read back through `DescribeApplicationVersions`, the reader a client waits
/// on, and reading it again reports the same thing. What a SINGLE version's
/// create answers, which bundle shapes reach which verdict and that a processed
/// version launches an environment are the suites above; this one is about a
/// fleet of them.
#[tokio::test]
async fn every_published_version_settles_in_the_status_its_own_bundle_validated_to() {
    let emulator = Emulator::with_object_storage();
    for application in [FLEET_APPLICATION, OTHER_APPLICATION] {
        given_application(&emulator, application).await;
    }

    // One bucket for every bundle, the way a client uploads into the one
    // storage location: the first upload creates it, the rest are PutObject
    // alone over the S3 front door.
    let mut bucket_created = false;
    for row in PUBLISHED {
        let case = row.case;
        let key = bundle_key(row);
        let bytes = row.bundle.bytes();
        if bucket_created {
            let stored = emulator
                .s3("PUT", &format!("/{BUNDLE_BUCKET}/{key}"), &bytes)
                .await;
            assert_eq!(
                stored, 200,
                "case {case:?}: expected 200 from PutObject of the bundle, got {stored}"
            );
        } else {
            emulator.put_object(BUNDLE_BUCKET, &key, &bytes).await;
            bucket_created = true;
        }

        let mut params = vec![
            ("ApplicationName", row.application),
            ("VersionLabel", row.label),
            ("SourceBundle.S3Bucket", BUNDLE_BUCKET),
            ("SourceBundle.S3Key", key.as_str()),
        ];
        if row.process {
            params.push(("Process", "true"));
        }
        let created = emulator.call("CreateApplicationVersion", &params).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateApplicationVersion, got {} and body {}",
            created.status, created.body
        );
        // A create that asked for pre-processing answers the transient status,
        // because the work it started is not done when the call returns; one
        // that asked for none answers its terminal status straight away.
        let at_the_create = eb::state::version_status_on_the_wire(if row.process {
            eb::state::VERSION_STATUS_PROCESSING
        } else {
            eb::state::VERSION_STATUS_UNPROCESSED
        });
        assert_eq!(
            created.value("ApplicationVersion/Status"),
            Some(at_the_create.as_str()),
            "case {case:?}: expected the create to report {at_the_create}, got body {}",
            created.body
        );
    }

    // Each version is waited on the way its own client waits on it: for the
    // CONDITION that it reports its terminal status, through the one poll this
    // test target states that wait in. The report it settled in is that
    // version's own labelled read, and reading it again must report the same
    // thing — a read reports the settled status without moving it on.
    for row in PUBLISHED {
        let case = row.case;
        let expected = eb::state::version_status_on_the_wire(row.terminal);
        let settled = wait_for_version_status(
            &emulator,
            case,
            row.application,
            row.label,
            row.terminal,
        )
        .await;
        assert_eq!(
            settled.value("ApplicationVersions/member/VersionLabel"),
            Some(row.label),
            "case {case:?}: the settled report must be the version that was asked for, got body {}",
            settled.body
        );
        let (again, answer) = described_status(&emulator, row.application, row.label).await;
        assert_eq!(
            again, expected,
            "case {case:?}: a read must not change what a later read sees, got {again:?} and body \
             {}",
            answer.body
        );
    }

    // And the report of the application as a whole pairs every one of its
    // versions with its OWN status.
    for application in [FLEET_APPLICATION, OTHER_APPLICATION] {
        let settled = emulator
            .call(
                "DescribeApplicationVersions",
                &[("ApplicationName", application)],
            )
            .await;
        let published: Vec<&Published> = PUBLISHED
            .iter()
            .filter(|row| row.application == application)
            .collect();
        assert_eq!(
            settled
                .values_at("ApplicationVersions/member/VersionLabel")
                .len(),
            published.len(),
            "case {application:?}: expected every published version to be reported, got body {}",
            settled.body
        );
        for row in published {
            let case = row.case;
            let expected = eb::state::version_status_on_the_wire(row.terminal);
            assert_eq!(
                reported_status(&settled, row.label),
                Some(expected.clone()),
                "case {case:?}: expected version {:?} of {application:?} to settle in {expected}, \
                 got body {}",
                row.label,
                settled.body
            );
        }
    }
}

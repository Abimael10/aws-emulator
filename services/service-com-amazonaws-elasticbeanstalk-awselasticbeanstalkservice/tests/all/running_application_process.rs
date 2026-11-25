// SPDX-License-Identifier: CC0-1.0

//! The PROCESS an environment runs the deployed application as, over the
//! service's own HTTP surface.
//!
//! The rule under test: an Elastic Beanstalk environment runs the source bundle's
//! web process as ONE long-lived process on its instance, so consecutive requests
//! to the address the environment publishes reach the SAME process — what the
//! application holds in memory is there for the next request — and that process is
//! replaced exactly where a deployment reaches the instances: an application server
//! restart ("causes the environment to restart the application container server
//! running on each Amazon EC2 instance"), an update whose deployment lands, or a
//! replacement of the instances themselves. An update that is CANCELLED reaches
//! them never, so it leaves the process they were running untouched.
//!
//! Beside it, the configuration the bundle CARRIES is applied by the deployment
//! before that process is started: an `option_settings:` block of a
//! `.ebextensions/*.config` file is merged into the environment's configuration
//! (with a value set directly on the environment winning over it), the `files:`
//! entries are written to the paths they declare and the `container_commands:`
//! really run, so their side effects are there for the application to read. An
//! environment launched from a saved configuration template alone runs that
//! template's configuration.
//!
//! Every assertion is driven through what the APPLICATION answers, so what is
//! under test is the process this emulator runs rather than anything this
//! emulator could have written itself: the probe below mints a boot identity once
//! per process, counts its own requests in memory, flips its own health, echoes
//! its process environment and reads a path of its instance.
//!
//! The harness (the router, the form encoder, the XML element reader, the
//! object-storage upload and the unsigned GET) is the one `lifecycle.rs` owns in
//! this same test binary.

use std::time::{Duration, Instant};

use crate::application_version_processing::zip_of;
use crate::deployed_application::solution_stack_of;
use crate::lifecycle::{wait_for_status, Answer, Emulator, POLL_INTERVAL, TEST_REGION};

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application every environment below is launched under.
const APPLICATION: &str = "running-process-app";

/// The bucket the suites upload a source bundle into.
const BUNDLE_BUCKET: &str = "eb-running-process-bundles";

/// The environment property an environment sets through the API, and its value.
const API_PROPERTY: &str = "PROBE_PRECEDENCE";
/// See [`API_PROPERTY`].
const API_VALUE: &str = "from-the-api";

/// How long a suite waits for the address to be served by the deployed
/// application.
///
/// Generous, and waited out by LOOKING AGAIN rather than by sleeping: starting a
/// process imports the standard library its server is built on, and a request may
/// wait for another run already in flight, so the condition is polled until it
/// holds.
const SERVED_DEADLINE: Duration = Duration::from_secs(180);

/// The statuses the plane RENDERS ITSELF for a request no run of the application
/// answered: an address with nothing behind it, and one whose serving did not
/// finish inside the span the socket allows. Neither is an answer of the
/// application's, so a suite polling for the program's own answer looks again past
/// them.
const NOT_THE_APPLICATIONS: &[u16] = &[502, 504];

/// The program the bundle's `Procfile` starts: a server that mints a boot identity
/// ONCE, counts its requests in memory, flips its own health, echoes its process
/// environment and reads a path of the instance it runs on.
///
/// Deployed and dialled through [`published`], [`launched`] and [`answered`], which
/// are `pub(crate)` so a suite beside this one that is about what a deployment
/// leaves ON that instance reads it back through this same probe instead of
/// carrying a second copy of it.
const PROBE_SERVER: &[u8] = br#"
import json
import os
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer

# Minted once per PROCESS: a value a request can only see again if it reached the same
# process that minted it.
BOOT = uuid.uuid4().hex

# Held in memory, so what one request leaves here is what the next one reads.
STATE = {"hits": 0, "failing": False}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path, _, query = self.path.partition("?")
        if path == "/boot":
            STATE["hits"] = STATE["hits"] + 1
            self.answer(200, json.dumps({"boot": BOOT, "hits": STATE["hits"]}))
            return
        if path == "/fail-on":
            STATE["failing"] = True
            self.answer(200, "failing")
            return
        if path == "/env":
            named = {}
            for name, value in os.environ.items():
                if name.startswith("PROBE_"):
                    named[name] = value
            self.answer(200, json.dumps(named, sort_keys=True))
            return
        if path == "/file":
            try:
                with open(query) as reading:
                    self.answer(200, reading.read())
            except OSError:
                self.answer(404, "no such file")
            return
        if STATE["failing"]:
            self.answer(500, "degraded %s" % BOOT)
            return
        self.answer(200, "healthy %s" % BOOT)

    def answer(self, status, body):
        encoded = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, format, *arguments):
        pass


print("probe: the web process started", flush=True)
HTTPServer(("", int(os.environ.get("PORT", "8000"))), Handler).serve_forever()
"#;

/// The `Procfile` that starts it, which is what makes the platform run the program
/// instead of its own default start command.
const PROCFILE: &[u8] = b"web: python3 -u server.py\n";

/// The same probe, for the OTHER runtime this emulator embeds an engine for: a
/// program a Node.js platform's start command runs, which starts a server of its
/// own, mints a boot identity once and counts its requests in memory.
const PROBE_SERVER_JS: &[u8] = br#"
const http = require('http');

// Minted once per PROCESS, exactly as the other runtime's probe mints it.
const BOOT = 'boot-' + Math.random().toString(16).slice(2);

let hits = 0;
let failing = false;

http.createServer(function (req, res) {
  const path = String(req.url || '/').split('?')[0];
  const answer = function (status, body) {
    res.writeHead(status, { 'Content-Type': 'text/plain; charset=utf-8' });
    res.end(body);
  };
  if (path === '/boot') {
    hits = hits + 1;
    answer(200, JSON.stringify({ boot: BOOT, hits: hits }));
    return;
  }
  if (path === '/fail-on') {
    failing = true;
    answer(200, 'failing');
    return;
  }
  if (failing) {
    answer(500, 'degraded ' + BOOT);
    return;
  }
  answer(200, 'healthy ' + BOOT);
}).listen(process.env.PORT);
console.log('probe: the web process started');
"#;

/// The `Procfile` that starts THAT program.
const PROCFILE_JS: &[u8] = b"web: node app.js\n";

/// One engine this emulator starts a deployed web process on, and the bundle that
/// probe is in: a row of
/// [`consecutive_requests_reach_one_long_lived_process`].
struct EngineRow {
    case: &'static str,
    /// The platform category the environment is launched on.
    platform_category: &'static str,
    /// The environment the row launches, which is also its CNAME prefix.
    environment: &'static str,
    /// The bundle's `Procfile` and the program it starts.
    procfile: &'static [u8],
    program: (&'static str, &'static [u8]),
}

/// One row per engine: the interpreter a Python platform's web command runs, and
/// the JavaScript engine a Node.js platform's does.
const ENGINE_ROWS: &[EngineRow] = &[
    EngineRow {
        case: "the interpreter a Python platform starts the program on",
        platform_category: eb::state::PLATFORM_CATEGORY_PYTHON,
        environment: "one-process-py",
        procfile: PROCFILE,
        program: ("server.py", PROBE_SERVER),
    },
    EngineRow {
        case: "the engine a Node.js platform starts the program on",
        platform_category: eb::state::PLATFORM_CATEGORY_NODEJS,
        environment: "one-process-js",
        procfile: PROCFILE_JS,
        program: ("app.js", PROBE_SERVER_JS),
    },
];

/// The directory the configuration below provisions on the instance.
const PROVISIONED_DIRECTORY: &str = "/tmp/eb-probe";

/// The file its `files:` section declares, and the content it declares for it.
const DECLARED_FILE: &str = "/tmp/eb-probe/written.txt";
/// See [`DECLARED_FILE`].
const DECLARED_CONTENT: &str = "written by the configuration\n";

/// The file its `container_commands:` section writes, and what that command writes
/// into it.
const COMMANDED_FILE: &str = "/tmp/eb-probe/commanded.txt";
/// See [`COMMANDED_FILE`].
const COMMANDED_CONTENT: &str = "ran-the-container-command";

/// A path no section of that configuration mentions, which therefore does not
/// exist on the instance.
const UNWRITTEN_FILE: &str = "/tmp/eb-probe/never-written.txt";

/// The environment property the bundle's configuration sets and the API does not,
/// and its value.
const BUNDLE_PROPERTY: &str = "PROBE_FROM_BUNDLE";
/// See [`BUNDLE_PROPERTY`].
const BUNDLE_VALUE: &str = "carried-by-the-bundle";

/// The bundle's own configuration file: an `option_settings:` block setting one
/// option the API does not set and one it does, a `files:` entry and a
/// `container_commands:` entry.
const EBEXTENSIONS: &[u8] = br#"
option_settings:
  aws:elasticbeanstalk:application:environment:
    PROBE_FROM_BUNDLE: carried-by-the-bundle
    PROBE_PRECEDENCE: from-the-bundle
files:
  "/tmp/eb-probe/written.txt":
    mode: "000644"
    owner: root
    group: root
    content: |
      written by the configuration
container_commands:
  01_write_the_token:
    command: printf ran-the-container-command > /tmp/eb-probe/commanded.txt
"#;

/// Call `action`, failing the named case unless it answered 200.
async fn ok(emulator: &Emulator, case: &str, action: &str, params: &[(&str, &str)]) -> Answer {
    let answer = emulator.call(action, params).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from {action}, got {} and body {}",
        answer.status, answer.body
    );
    answer
}

/// The members `OptionSettings` travels as on the awsQuery wire, one per
/// `(namespace, option, value)` triple.
fn option_members(settings: &[(&str, &str, &str)]) -> Vec<(String, String)> {
    settings
        .iter()
        .enumerate()
        .flat_map(|(index, (namespace, option, value))| {
            let member = index + 1;
            [
                (
                    format!("OptionSettings.member.{member}.Namespace"),
                    (*namespace).to_string(),
                ),
                (
                    format!("OptionSettings.member.{member}.OptionName"),
                    (*option).to_string(),
                ),
                (
                    format!("OptionSettings.member.{member}.Value"),
                    (*value).to_string(),
                ),
            ]
        })
        .collect()
}

/// Upload a bundle carrying the probe server `program` started by `procfile`,
/// beside `files`, and create the application and the version `version` from it.
async fn published_bundle(
    emulator: &Emulator,
    case: &str,
    version: &str,
    procfile: &[u8],
    program: (&str, &[u8]),
    files: &[(&str, &[u8])],
) {
    let key = format!("bundles/{version}.zip");
    let mut entries: Vec<(&str, &[u8])> = vec![("Procfile", procfile), program];
    entries.extend_from_slice(files);
    emulator
        .put_object(BUNDLE_BUCKET, &key, &zip_of(&entries))
        .await;
    // The application is created once per emulator; a second create of the same
    // name is refused, which says nothing about this suite.
    let _ = emulator
        .call("CreateApplication", &[("ApplicationName", APPLICATION)])
        .await;
    ok(
        emulator,
        case,
        "CreateApplicationVersion",
        &[
            ("ApplicationName", APPLICATION),
            ("VersionLabel", version),
            ("SourceBundle.S3Bucket", BUNDLE_BUCKET),
            ("SourceBundle.S3Key", &key),
        ],
    )
    .await;
}

/// [`published_bundle`] for the Python probe, which every suite but the engine
/// table deploys.
pub(crate) async fn published(
    emulator: &Emulator,
    case: &str,
    version: &str,
    files: &[(&str, &[u8])],
) {
    published_bundle(
        emulator,
        case,
        version,
        PROCFILE,
        ("server.py", PROBE_SERVER),
        files,
    )
    .await;
}

/// Launch an environment named `prefix` (its CNAME prefix too) deploying `version`
/// on a platform of `platform_category` with `settings`, and wait out the launch
/// the clock owes.
async fn launched_on(
    emulator: &Emulator,
    case: &str,
    prefix: &str,
    version: &str,
    platform_category: &str,
    settings: &[(&str, &str, &str)],
) {
    let solution_stack = solution_stack_of(platform_category);
    let members = option_members(settings);
    let mut params: Vec<(&str, &str)> = vec![
        ("ApplicationName", APPLICATION),
        ("EnvironmentName", prefix),
        ("CNAMEPrefix", prefix),
        ("SolutionStackName", &solution_stack),
        ("VersionLabel", version),
    ];
    params.extend(
        members
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    ok(emulator, case, "CreateEnvironment", &params).await;
    // The launch the clock owes is waited for by LOOKING AGAIN until the
    // environment reports it is ready to serve, never by waiting a fixed time.
    wait_for_status(emulator, case, prefix, eb::state::STATUS_READY).await;
}

/// [`launched_on`] on the Python platform, which every suite but the engine table
/// launches on.
pub(crate) async fn launched(
    emulator: &Emulator,
    case: &str,
    prefix: &str,
    version: &str,
    settings: &[(&str, &str, &str)],
) {
    launched_on(
        emulator,
        case,
        prefix,
        version,
        eb::state::PLATFORM_CATEGORY_PYTHON,
        settings,
    )
    .await;
}

/// GET `path` of the published address of the environment with the CNAME prefix
/// `prefix`, looking again until the deployed application has ANSWERED — a request
/// this emulator had no run slot for is answered as an address with nothing behind
/// it, which is not an answer of the application's.
pub(crate) async fn answered(emulator: &Emulator, case: &str, prefix: &str, path: &str) -> Answer {
    let url = format!(
        "http://{}{path}",
        emulator.published_cname(TEST_REGION, prefix)
    );
    let deadline = Instant::now() + SERVED_DEADLINE;
    loop {
        let served = emulator.get_url(&url).await;
        if !NOT_THE_APPLICATIONS.contains(&served.status) || Instant::now() >= deadline {
            assert!(
                !NOT_THE_APPLICATIONS.contains(&served.status),
                "case {case:?}: expected {url} to be answered by the deployed application, got \
                 {} and body {}",
                served.status,
                served.body
            );
            return served;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What the probe reports at `/boot`: the identity the process minted when it
/// started, and how many times it has been asked.
async fn boot(emulator: &Emulator, case: &str, prefix: &str) -> (String, u64) {
    let answer = answered(emulator, case, prefix, "/boot").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the probe's own answer at /boot, got {} and body {}",
        answer.status, answer.body
    );
    let reported: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_else(|failure| {
        panic!(
            "case {case:?}: expected the probe's report at /boot, got {:?} ({failure})",
            answer.body
        )
    });
    let boot = reported
        .get("boot")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a boot identity, got {reported}"))
        .to_string();
    let hits = reported
        .get("hits")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a request count, got {reported}"));
    (boot, hits)
}

/// RULE: consecutive requests to the address an environment publishes reach ONE
/// long-lived process, so a counter the application holds in memory increases
/// across them and a route that flips the application's own state changes what the
/// next request is answered with — whichever engine this emulator starts the
/// bundle's web process on.
#[tokio::test]
async fn consecutive_requests_reach_one_long_lived_process() {
    for row in ENGINE_ROWS {
        let case = row.case;
        let emulator = Emulator::with_object_storage();
        published_bundle(&emulator, case, "v1", row.procfile, row.program, &[]).await;
        launched_on(
            &emulator,
            case,
            row.environment,
            "v1",
            row.platform_category,
            &[],
        )
        .await;

        // Three reads of the counter the application holds IN MEMORY: strictly
        // increasing, and all three from the one process that minted its identity.
        let mut counted: Vec<u64> = Vec::new();
        let mut identities: Vec<String> = Vec::new();
        for _ in 0..3 {
            let (boot, hits) = boot(&emulator, case, row.environment).await;
            counted.push(hits);
            identities.push(boot);
        }
        assert!(
            counted.windows(2).all(|pair| pair[1] > pair[0]),
            "case {case:?}: expected the in-memory counter to increase across requests, got \
             {counted:?}"
        );
        assert!(
            identities.windows(2).all(|pair| pair[0] == pair[1]),
            "case {case:?}: expected every request to reach the process that minted one \
             identity, got {identities:?}"
        );

        // A route that flips the application's own state changes what a LATER
        // request is answered with, which is only observable if that state
        // survived.
        let flipped = answered(&emulator, case, row.environment, "/fail-on").await;
        assert_eq!(
            flipped.status, 200,
            "case {case:?}: expected the probe to accept the flip, got {} and body {}",
            flipped.status, flipped.body
        );
        let degraded = answered(&emulator, case, row.environment, "/").await;
        assert_eq!(
            degraded.status, 500,
            "case {case:?}: expected the application's own 500 after the flip, got {} and body \
             {}",
            degraded.status, degraded.body
        );
        assert!(
            degraded.body.starts_with("degraded ") && degraded.body.contains(&identities[0]),
            "case {case:?}: expected the degraded answer of the same process, got {:?}",
            degraded.body
        );
    }
}

/// What one call does to the process an environment's instance runs: a row of
/// [`the_process_is_replaced_only_where_a_deployment_reaches_the_instances`].
struct ReplacementRow {
    case: &'static str,
    /// The environment the row launches, which is also its CNAME prefix.
    environment: &'static str,
    /// Whether the call it makes replaces the process the instance was running.
    replaces: bool,
}

/// The rows: the two calls that put a new process on the instances, and the one
/// that withholds the change it was asked for.
const REPLACEMENT_ROWS: &[ReplacementRow] = &[
    ReplacementRow {
        case: "an application server restart",
        environment: "restarted-env",
        replaces: true,
    },
    ReplacementRow {
        case: "an update whose deployment lands",
        environment: "updated-env",
        replaces: true,
    },
    ReplacementRow {
        case: "an update that is cancelled",
        environment: "aborted-env",
        replaces: false,
    },
];

/// Make the call the row names, and answer once the environment is ready again.
async fn acted(emulator: &Emulator, row: &ReplacementRow) {
    let case = row.case;
    match case {
        "an application server restart" => {
            ok(
                emulator,
                case,
                "RestartAppServer",
                &[("EnvironmentName", row.environment)],
            )
            .await;
        }
        "an update whose deployment lands" => {
            let members = option_members(&[(
                eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
                "PROBE_DEPLOYED",
                "by-the-update",
            )]);
            let mut params: Vec<(&str, &str)> = vec![("EnvironmentName", row.environment)];
            params.extend(
                members
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            ok(emulator, case, "UpdateEnvironment", &params).await;
        }
        _ => {
            let members = option_members(&[(
                eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
                "PROBE_DEPLOYED",
                "by-the-cancelled-update",
            )]);
            let mut params: Vec<(&str, &str)> = vec![("EnvironmentName", row.environment)];
            params.extend(
                members
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            ok(emulator, case, "UpdateEnvironment", &params).await;
            // "Cancels in-progress environment configuration update": the change
            // the update was deploying never reaches the instances.
            ok(
                emulator,
                case,
                "AbortEnvironmentUpdate",
                &[("EnvironmentName", row.environment)],
            )
            .await;
        }
    }
    // Ready again — waited for as a CONDITION, never as a fixed time.
    wait_for_status(emulator, case, row.environment, eb::state::STATUS_READY).await;
}

/// RULE: the process an environment's instance runs is replaced exactly where a
/// deployment reaches that instance — an application server restart and an update
/// whose deployment lands — and an update that is CANCELLED leaves the instance
/// running the process it was running, which the identity that process minted when
/// it started is what shows.
#[tokio::test]
async fn the_process_is_replaced_only_where_a_deployment_reaches_the_instances() {
    for row in REPLACEMENT_ROWS {
        let case = row.case;
        let emulator = Emulator::with_object_storage();
        published(&emulator, case, "v1", &[]).await;
        launched(&emulator, case, row.environment, "v1", &[]).await;

        let (before, first) = boot(&emulator, case, row.environment).await;
        assert_eq!(
            first, 1,
            "case {case:?}: expected the first request of a fresh process, got {first}"
        );
        acted(&emulator, row).await;
        let (after, counted) = boot(&emulator, case, row.environment).await;
        if row.replaces {
            assert_ne!(
                after, before,
                "case {case:?}: expected a new process to have been started, got the identity \
                 {before:?} again"
            );
            assert_eq!(
                counted, 1,
                "case {case:?}: expected a fresh process to count from its own first request, \
                 got {counted}"
            );
        } else {
            assert_eq!(
                after, before,
                "case {case:?}: expected the instance to keep the process it was running, got \
                 {after:?} where it had minted {before:?}"
            );
            assert!(
                counted > first,
                "case {case:?}: expected the kept process to go on counting, got {counted} \
                 after {first}"
            );
        }
    }
}

/// RULE: the configuration a source bundle CARRIES is applied by the deployment —
/// an `option_settings:` entry of its `.ebextensions` is in the process
/// environment unless the environment itself sets the same option, the `files:` it
/// declares are written with the content they declare, and the
/// `container_commands:` really run — while a path no section of it mentions does
/// not exist on the instance.
#[tokio::test]
async fn the_bundles_own_configuration_is_applied_before_the_process_starts() {
    let case = "a bundle carrying its own configuration";
    let emulator = Emulator::with_object_storage();
    published(
        &emulator,
        case,
        "v1",
        &[(".ebextensions/01-probe.config", EBEXTENSIONS)],
    )
    .await;
    launched(
        &emulator,
        case,
        "configured-env",
        "v1",
        &[(
            eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
            API_PROPERTY,
            API_VALUE,
        )],
    )
    .await;

    // The process environment, as the application itself reports it.
    let reported = answered(&emulator, case, "configured-env", "/env").await;
    let seen: serde_json::Value = serde_json::from_str(&reported.body).unwrap_or_else(|failure| {
        panic!(
            "case {case:?}: expected the probe's report of its environment, got {:?} ({failure})",
            reported.body
        )
    });
    let expected = serde_json::json!({
        // Set ONLY in the bundle's configuration, and in effect because the
        // deployment merged it in.
        BUNDLE_PROPERTY: BUNDLE_VALUE,
        // Set in BOTH, where the value applied to the environment itself wins.
        API_PROPERTY: API_VALUE,
    });
    assert_eq!(
        seen, expected,
        "case {case:?}: expected the bundle's option in effect and the API's value winning, got \
         {seen}"
    );

    // What the deployment applied on the instance, as the application reads it
    // back off that instance.
    let rows: &[(&str, &str, u16, &str)] = &[
        (
            "the path the files: section declares",
            DECLARED_FILE,
            200,
            DECLARED_CONTENT,
        ),
        (
            "the path the container command wrote",
            COMMANDED_FILE,
            200,
            COMMANDED_CONTENT,
        ),
        (
            "a path no section of the configuration mentions",
            UNWRITTEN_FILE,
            404,
            "no such file",
        ),
    ];
    for (row, path, status, body) in rows {
        let read = answered(
            &emulator,
            row,
            "configured-env",
            &format!("/file?{path}"),
        )
        .await;
        assert_eq!(
            read.status, *status,
            "case {row:?}: expected {status} for {path}, got {} and body {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body, *body,
            "case {row:?}: expected {body:?} at {path}, got {:?}",
            read.body
        );
    }
    assert!(
        PROVISIONED_DIRECTORY.len() < DECLARED_FILE.len(),
        "the declared file is under the directory the configuration provisions"
    );
}

/// RULE: the enhanced-health reports are about the traffic the application really
/// answered — `ApplicationMetrics` counts the requests it served and the status
/// codes it answered them with, and an environment whose application is failing
/// its requests is reported `Degraded` (Red) rather than `Ok` (Green), which is
/// what the health agent on the instance is reporting.
#[tokio::test]
async fn the_health_reports_what_the_application_answered() {
    let case = "an application that starts failing its requests";
    let emulator = Emulator::with_object_storage();
    published(&emulator, case, "v1", &[]).await;
    launched(&emulator, case, "degrading-env", "v1", &[]).await;

    // A healthy answer first: the agent reports no problem it has not observed.
    let healthy = answered(&emulator, case, "degrading-env", "/").await;
    assert_eq!(
        healthy.status, 200,
        "case {case:?}: expected the application's own answer, got {} and body {}",
        healthy.status, healthy.body
    );
    let reported = health_of(&emulator, case, "degrading-env").await;
    assert_eq!(
        (reported.0.as_str(), reported.1.as_str()),
        (eb::state::HEALTH_GREEN, eb::state::HEALTH_STATUS_OK),
        "case {case:?}: expected the environment to report it is healthy, got {reported:?}"
    );
    let (requests, failures) = request_counts(&emulator, case, "degrading-env").await;
    assert_eq!(
        (requests.as_str(), failures.as_str()),
        ("1", "0"),
        "case {case:?}: expected the metrics to count the one request the application answered"
    );

    // The application is driven into failing its requests, which is what the agent
    // then reports.
    answered(&emulator, case, "degrading-env", "/fail-on").await;
    let broken = answered(&emulator, case, "degrading-env", "/").await;
    assert_eq!(
        broken.status, 500,
        "case {case:?}: expected the application's own 500, got {} and body {}",
        broken.status, broken.body
    );
    let reported = health_of(&emulator, case, "degrading-env").await;
    assert_eq!(
        (reported.0.as_str(), reported.1.as_str()),
        (eb::state::HEALTH_RED, eb::state::INSTANCE_HEALTH_DEGRADED),
        "case {case:?}: expected the failures the application answered to be reported, got \
         {reported:?}"
    );
    let (requests, failures) = request_counts(&emulator, case, "degrading-env").await;
    assert_ne!(
        failures, "0",
        "case {case:?}: expected the 5xx the application answered to be counted, got {requests} \
         requests and {failures} of them 5xx"
    );
    // An application failing more requests than it answers is a SEVERE instance,
    // which is the band above a degraded one.
    let broken = answered(&emulator, case, "degrading-env", "/").await;
    assert_eq!(
        broken.status, 500,
        "case {case:?}: expected the application to go on failing, got {} and body {}",
        broken.status, broken.body
    );
    let reported = health_of(&emulator, case, "degrading-env").await;
    assert_eq!(
        (reported.0.as_str(), reported.1.as_str()),
        (eb::state::HEALTH_RED, eb::state::INSTANCE_HEALTH_SEVERE),
        "case {case:?}: expected an application failing most of its requests to be graded \
         severe, got {reported:?}"
    );
    // And the environment itself reports the same health, so a client reading
    // either is told the same thing.
    let described = ok(
        &emulator,
        case,
        "DescribeEnvironments",
        &[("EnvironmentNames.member.1", "degrading-env")],
    )
    .await;
    assert_eq!(
        described.value("Environments/member/Health"),
        Some(eb::state::HEALTH_RED),
        "case {case:?}: every read must report the same health, got body {}",
        described.body
    );
}

/// The health COLOUR and health STATUS `DescribeEnvironmentHealth` reports for the
/// environment `environment_name`.
async fn health_of(emulator: &Emulator, case: &str, environment_name: &str) -> (String, String) {
    let reported = ok(
        emulator,
        case,
        "DescribeEnvironmentHealth",
        &[
            ("EnvironmentName", environment_name),
            ("AttributeNames.member.1", eb::state::HEALTH_ATTRIBUTE_ALL),
        ],
    )
    .await;
    (
        reported.expect("Color", case).to_string(),
        reported.expect("HealthStatus", case).to_string(),
    )
}

/// The `RequestCount` and the 5xx count `DescribeEnvironmentHealth` reports in its
/// `ApplicationMetrics` for that environment.
async fn request_counts(
    emulator: &Emulator,
    case: &str,
    environment_name: &str,
) -> (String, String) {
    let reported = ok(
        emulator,
        case,
        "DescribeEnvironmentHealth",
        &[
            ("EnvironmentName", environment_name),
            ("AttributeNames.member.1", eb::state::HEALTH_ATTRIBUTE_ALL),
        ],
    )
    .await;
    (
        reported
            .value("ApplicationMetrics/RequestCount")
            .unwrap_or_default()
            .to_string(),
        reported
            .value("ApplicationMetrics/StatusCodes/Status5xx")
            .unwrap_or_default()
            .to_string(),
    )
}

/// RULE: an environment created from a saved configuration template alone takes
/// its platform AND its option settings from that template, so a property the
/// template sets is in the application's process environment — and the template is
/// a SNAPSHOT, so a later `UpdateConfigurationTemplate` changes nothing the running
/// environment serves.
#[tokio::test]
async fn an_environment_launched_from_a_template_runs_the_templates_configuration() {
    let case = "a saved configuration template";
    let emulator = Emulator::with_object_storage();
    published(&emulator, case, "v1", &[]).await;

    let solution_stack = solution_stack_of(eb::state::PLATFORM_CATEGORY_PYTHON);
    let members = option_members(&[(
        eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
        "PROBE_FROM_TEMPLATE",
        "stored-in-the-template",
    )]);
    let mut params: Vec<(&str, &str)> = vec![
        ("ApplicationName", APPLICATION),
        ("TemplateName", "probe-template"),
        ("SolutionStackName", &solution_stack),
    ];
    params.extend(
        members
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    ok(&emulator, case, "CreateConfigurationTemplate", &params).await;

    // No SolutionStackName and no OptionSettings: both come from the template.
    ok(
        &emulator,
        case,
        "CreateEnvironment",
        &[
            ("ApplicationName", APPLICATION),
            ("EnvironmentName", "template-env"),
            ("CNAMEPrefix", "template-env"),
            ("TemplateName", "probe-template"),
            ("VersionLabel", "v1"),
        ],
    )
    .await;
    wait_for_status(&emulator, case, "template-env", eb::state::STATUS_READY).await;

    let reported = answered(&emulator, case, "template-env", "/env").await;
    let expected = serde_json::json!({ "PROBE_FROM_TEMPLATE": "stored-in-the-template" });
    let seen: serde_json::Value = serde_json::from_str(&reported.body).unwrap_or_else(|failure| {
        panic!(
            "case {case:?}: expected the probe's report of its environment, got {:?} ({failure})",
            reported.body
        )
    });
    assert_eq!(
        seen, expected,
        "case {case:?}: expected the template's own option in the process environment, got {seen}"
    );

    // The template is a snapshot: changing it changes nothing the launched
    // environment runs.
    let changed = option_members(&[(
        eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
        "PROBE_FROM_TEMPLATE",
        "changed-after-the-launch",
    )]);
    let mut params: Vec<(&str, &str)> = vec![
        ("ApplicationName", APPLICATION),
        ("TemplateName", "probe-template"),
    ];
    params.extend(
        changed
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    ok(&emulator, case, "UpdateConfigurationTemplate", &params).await;

    let again = answered(&emulator, case, "template-env", "/env").await;
    let seen: serde_json::Value = serde_json::from_str(&again.body).unwrap_or_else(|failure| {
        panic!(
            "case {case:?}: expected the probe's report of its environment, got {:?} ({failure})",
            again.body
        )
    });
    assert_eq!(
        seen, expected,
        "case {case:?}: a template is a snapshot, so the running environment must be unchanged, \
         got {seen}"
    );
}

// SPDX-License-Identifier: CC0-1.0

//! The page a web-server PACKAGE ships at its own document root, as the guest that installed it
//! serves it — driven over the wire the service speaks, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! RULE: a package manager installs a web server together with the directory its shipped
//! configuration serves AND the page that distribution ships inside it, so a guest whose
//! user-data program installs a server and starts it serves that page at the directory's root
//! before anything of the guest has written a page of its own — and the server's own `404` for
//! a path the directory holds no document at. A program that writes its own document at that
//! path REPLACES the shipped one.
//!
//! This is what makes an "install a web server and start it" program — the shape the EC2 user
//! guide's own tutorial has, and the shape a load balancer's health check of `/` is answered by
//! ([`super::instance_guest_web_server`] drives the serving itself) — answer a request at all
//! rather than serve an empty directory.
//!
//! The rows differ along what the rule generalises over: which package is installed and which
//! document root it ships (the Apache package of the Amazon Linux family, the Debian family's
//! name for the same server, nginx with its own root), the page each of them ships, a program
//! that writes its own document at that root afterwards, and a path the root holds no document
//! at.

use std::time::{Duration, Instant};

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The marker the one row whose program writes its own page puts into it.
const OWN_PAGE: &str = "own-page-not-the-shipped-one";

/// The port every package of the catalog ships its configuration listening on.
const SERVED_PORT: i32 = 80;

/// The path a server answers its document root's index document at, and a path no row's root
/// holds a document at.
const ROOT_PATH: &str = "/";
const MISSING_PATH: &str = "/this-path-does-not-exist";

/// The status a static server answers the document it was asked for with, and the one it
/// answers a path its document root holds nothing at.
const SERVED_STATUS: u16 = 200;
const NOT_FOUND_STATUS: u16 = 404;

/// How long a row waits for the guest to come up and its program to run, and how often it
/// looks. Generous on purpose — many times the span a launch owes its boot — because the
/// deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One row: the user data a launch carries, and what the guest must then answer at its server's
/// root.
struct Row {
    case: &'static str,
    user_data: &'static str,
    /// A fragment the body answered for the root must carry.
    carries: &'static str,
    /// A fragment that body must NOT carry — empty where the row asserts nothing of the kind.
    carries_not: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the Apache package of the Amazon Linux family, installed and started",
        user_data: "#!/bin/bash\nyum install -y httpd\nsystemctl start httpd\n",
        // The one-line document the Apache HTTP Server's own distribution ships at its
        // document root.
        carries: "It works!",
        carries_not: "",
    },
    Row {
        case: "the Debian family's name for the same server, started through its wrapper",
        user_data: "#!/bin/sh\napt-get install -y apache2\nservice apache2 start\n",
        carries: "It works!",
        carries_not: "",
    },
    Row {
        case: "nginx, enabled and started at once, over its own document root",
        user_data: "#!/bin/bash\ndnf install -y nginx\nsystemctl enable --now nginx\n",
        // The welcome page nginx's own distribution ships at its document root.
        carries: "Welcome to nginx!",
        carries_not: "",
    },
    Row {
        case: "a program that writes its own page at that root replaces the shipped one",
        user_data: "#!/bin/bash\nyum install -y httpd\nsystemctl start httpd\n\
                    echo own-page-not-the-shipped-one > /var/www/html/index.html\n",
        carries: OWN_PAGE,
        carries_not: "It works!",
    },
];

/// One emulator over its own engine, with one AMI of the calling account seeded the way a
/// register operation would store it — answered beside its image id and that engine.
fn emulator() -> (
    MultiServiceRouter,
    std::sync::Arc<engine::InMemoryEngine>,
    String,
) {
    let (router, engine) = super::ec2_query::emulator();
    let image_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard.insert_image(ImageSpec {
            name: "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64".to_string(),
            ..ImageSpec::default()
        })
    };
    (router, engine, image_id)
}

/// Launch one instance whose user data is `user_data`, answering its id.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    user_data: &str,
) -> String {
    let params = vec![
        p("ImageId", image_id),
        p("InstanceType", "t3.micro"),
        p("MinCount", "1"),
        p("MaxCount", "1"),
        p(
            "UserData",
            base64::engine::general_purpose::STANDARD.encode(user_data),
        ),
    ];
    let (status, body) = call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status}: {body}"
    );
    elements(&body, "instanceId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// What the guest of `instance_id` answers for a `GET path` on the served port, as `(status,
/// body)` — `None` when nothing of that guest is listening there.
fn served(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    instance_id: &str,
    path: &str,
) -> Option<(u16, String)> {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    guard
        .serve_instance_guest_http(instance_id, SERVED_PORT, "GET", path)
        .map(|answered| {
            (
                answered.status,
                String::from_utf8_lossy(&answered.body).into_owned(),
            )
        })
}

/// Look at the instance through the API until its guest has booted and its program has left the
/// server listening — the boot a launch owes is materialised by that very look — and answer what
/// the guest then serves for `path`.
///
/// Polls the CONDITION rather than waiting a fixed time.
async fn serving(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    instance_id: &str,
    path: &str,
) -> (u16, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = call(
            router,
            REGION,
            "DescribeInstances",
            &[p("InstanceId.1", instance_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInstances must succeed, got {status}: {body}"
        );
        if let Some(answered) = served(engine, instance_id, path) {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: nothing of the guest answered a {path} on port {SERVED_PORT}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// RULE: a guest whose program installs a web server and starts it serves the page that
/// package's own distribution ships at its document root, until the guest's own program writes
/// a document there.
#[tokio::test]
async fn an_installed_server_serves_the_page_its_package_ships() {
    let (router, engine, image_id) = emulator();
    for row in ROWS {
        let instance_id = launch(&router, row.case, &image_id, row.user_data).await;
        let (status, body) = serving(&router, &engine, row.case, &instance_id, ROOT_PATH).await;
        assert_eq!(
            status, SERVED_STATUS,
            "case {:?}: expected HTTP {SERVED_STATUS} for {ROOT_PATH}, got {status} with body \
             {body:?}",
            row.case
        );
        assert!(
            body.contains(row.carries),
            "case {:?}: expected the body for {ROOT_PATH} to carry {:?}, got {body:?}",
            row.case,
            row.carries
        );
        if !row.carries_not.is_empty() {
            assert!(
                !body.contains(row.carries_not),
                "case {:?}: the program's own page must replace the shipped one, but the body \
                 for {ROOT_PATH} carries {:?}: {body:?}",
                row.case,
                row.carries_not
            );
        }
        // The server answers its OWN status for a path its document root holds no document at:
        // what the package ships is one page, not a page for every path.
        let missing = served(&engine, &instance_id, MISSING_PATH)
            .unwrap_or_else(|| panic!("case {:?}: the server must answer {MISSING_PATH}", row.case));
        assert_eq!(
            missing.0, NOT_FOUND_STATUS,
            "case {:?}: expected HTTP {NOT_FOUND_STATUS} for {MISSING_PATH}, got {missing:?}",
            row.case
        );
    }
}

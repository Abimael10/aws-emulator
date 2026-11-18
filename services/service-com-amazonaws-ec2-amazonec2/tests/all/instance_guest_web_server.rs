// SPDX-License-Identifier: CC0-1.0

//! The WEB SERVER an instance's own user data installs and starts, driven over the wire the
//! service speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! RULE: an instance's init system RUNS that instance's user data when the guest comes up,
//! so a program that INSTALLS a web server and STARTS its unit leaves that server listening
//! on the port and serving the directory the installed package ships — and a request to that
//! port is answered out of the guest's own storage: the document the request's path names
//! under that directory, as the guest's own program wrote it, and the status a static server
//! answers for a path the directory holds nothing at. A guest whose program started no server
//! is listening on nothing, so nothing answers on any port of it.
//!
//! What a client does with that server is send it traffic, and the one in this emulator that
//! does is the load balancer in front of the instance, which reaches it through the state seam
//! asserted here (`service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10`'s
//! `tests/all/alb_instance_target.rs` drives that end to end). The launch, the boot and the
//! program are all driven through this service's own API.
//!
//! The rows differ along what the rule generalises over: which server the program installs
//! (the Apache package of the Amazon Linux family, the Debian family's name for it, nginx —
//! each with its own document root), how the program starts it (the init system's control
//! program, that program's `--now`, the System V wrapper, the interpreter's own HTTP server
//! module on a port of its own, that module behind a wrapper over a document the document's
//! own `write_files` module wrote), how the program writes the document it serves (a
//! redirection, a here-document, the `write_files` module) and the programs that leave
//! nothing listening (an install with no start, a start of a server nothing installed, no
//! user data at all).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The marker the guest's own program writes into the document it serves, so a served body
/// can be told from anything else.
const MARKER: &str = "served-by-the-guest-7f3a";

/// How long a row waits for the guest to come up and its program to run, and how often it
/// looks. Generous on purpose — many times the span a launch owes its boot — because the
/// deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

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
    user_data: Option<&str>,
) -> String {
    let mut params = vec![
        p("ImageId", image_id),
        p("InstanceType", "t3.micro"),
        p("MinCount", "1"),
        p("MaxCount", "1"),
    ];
    if let Some(user_data) = user_data {
        params.push(p(
            "UserData",
            base64::engine::general_purpose::STANDARD.encode(user_data),
        ));
    }
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

/// Look at the instance through the API until its guest has booted — which is the write that
/// runs its user-data program — and answer the ports it is then listening on.
///
/// The look is `DescribeInstances`, so the boot the launch owes is materialised exactly as it
/// is for any other caller; the ports are read off this service's own store, since nothing of
/// the EC2 API reports what a guest is listening on.
async fn listening_ports(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    instance_id: &str,
    expected: usize,
) -> Vec<i32> {
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
            "DescribeInstances must succeed, got {status}: {body}"
        );
        let booted = {
            let store = engine.state::<Ec2State>();
            let guard = store.lock();
            let ports = guard.instance_guest_server_ports(instance_id);
            let booted = guard
                .instances
                .get(instance_id)
                .is_some_and(|instance| !instance.boots.is_empty());
            (booted && ports.len() >= expected).then_some(ports)
        };
        if let Some(ports) = booted {
            return ports;
        }
        if Instant::now() >= deadline {
            let store = engine.state::<Ec2State>();
            let guard = store.lock();
            return guard.instance_guest_server_ports(instance_id);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What the guest of `instance_id` answers for a `GET path` on `port`, as `(status, body,
/// content type)` — `None` when nothing of that guest is listening there.
fn served(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    instance_id: &str,
    port: i32,
    path: &str,
) -> Option<(u16, String, Option<String>)> {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    guard
        .serve_instance_guest_http(instance_id, port, "GET", path)
        .map(|answered| {
            (
                answered.status,
                String::from_utf8_lossy(&answered.body).into_owned(),
                answered.content_type,
            )
        })
}

/// One row: the user data a launch carries, and what the guest then serves.
struct Row {
    case: &'static str,
    user_data: Option<String>,
    /// The port the guest must be listening on, and the document root that server serves —
    /// `None` for a program that leaves nothing listening.
    listening: Option<i32>,
    /// The body a `GET /` on that port must answer with, up to the line ending the program's
    /// own `echo` wrote.
    body: String,
}

/// RULE: a guest whose user-data program installs a web server and starts it serves that
/// package's own document root on that package's own port, out of the guest's own storage; a
/// program that starts nothing leaves the guest listening on nothing.
#[tokio::test]
async fn a_guest_serves_the_web_server_its_user_data_started() {
    let rows: Vec<Row> = vec![
        Row {
            case: "the Apache package of the Amazon Linux family, started by its unit",
            user_data: Some(format!(
                "#!/bin/bash\n\
                 yum update -y\n\
                 yum install -y httpd\n\
                 systemctl start httpd\n\
                 echo \"<h1>{MARKER}</h1>\" > /var/www/html/index.html\n"
            )),
            listening: Some(80),
            body: format!("<h1>{MARKER}</h1>"),
        },
        Row {
            case: "nginx, enabled and started at once, serving its own document root",
            user_data: Some(format!(
                "#cloud-config\n\
                 runcmd:\n\
                 \x20 - dnf install -y nginx\n\
                 \x20 - systemctl enable --now nginx\n\
                 \x20 - echo \"{MARKER}\" > /usr/share/nginx/html/index.html\n"
            )),
            listening: Some(80),
            body: MARKER.to_string(),
        },
        Row {
            case: "the Debian family's name for Apache, started through the System V wrapper",
            user_data: Some(format!(
                "#!/bin/sh\n\
                 apt-get install -y apache2\n\
                 service apache2 start\n\
                 cat > /var/www/html/index.html <<'PAGE'\n\
                 {MARKER}\n\
                 PAGE\n"
            )),
            listening: Some(80),
            body: MARKER.to_string(),
        },
        Row {
            case: "the interpreter's own HTTP server module, on a port of its own",
            user_data: Some(format!(
                "#!/bin/bash\n\
                 mkdir -p /srv/site\n\
                 echo \"{MARKER}\" > /srv/site/index.html\n\
                 python3 -m http.server 8080 --directory /srv/site\n"
            )),
            listening: Some(8080),
            body: MARKER.to_string(),
        },
        Row {
            case: "that module started through a wrapper, over a document the config wrote",
            user_data: Some(format!(
                "#cloud-config\n\
                 write_files:\n\
                 \x20 - path: /srv/pages/index.html\n\
                 \x20   content: {MARKER}\n\
                 runcmd:\n\
                 \x20 - nohup python3 -m http.server 8080 --directory /srv/pages &\n"
            )),
            listening: Some(8080),
            body: MARKER.to_string(),
        },
        Row {
            case: "a program that installs a server and never starts it",
            user_data: Some(
                "#!/bin/bash\nyum install -y httpd\necho installed > /var/www/html/index.html\n"
                    .to_string(),
            ),
            listening: None,
            body: String::new(),
        },
        Row {
            case: "a program that starts a server nothing installed",
            user_data: Some("#!/bin/bash\nsystemctl start httpd\n".to_string()),
            listening: None,
            body: String::new(),
        },
        Row {
            case: "no user data at all",
            user_data: None,
            listening: None,
            body: String::new(),
        },
    ];

    let (router, engine, image_id) = emulator();
    for row in &rows {
        let instance_id = launch(&router, row.case, &image_id, row.user_data.as_deref()).await;
        let expected = usize::from(row.listening.is_some());
        let ports = listening_ports(&router, &engine, &instance_id, expected).await;
        let Some(port) = row.listening else {
            assert!(
                ports.is_empty(),
                "case {:?}: expected the guest to be listening on nothing, got {ports:?}",
                row.case
            );
            assert!(
                served(&engine, &instance_id, 80, "/").is_none(),
                "case {:?}: nothing may answer on a port no server of the guest listens on",
                row.case
            );
            continue;
        };
        assert_eq!(
            ports,
            vec![port],
            "case {:?}: expected the guest to be listening on {port}, got {ports:?}",
            row.case
        );
        let answered = served(&engine, &instance_id, port, "/");
        let Some((status, body, content_type)) = answered else {
            panic!(
                "case {:?}: expected the guest's own document on port {port}, got nothing",
                row.case
            );
        };
        assert_eq!(
            status, 200,
            "case {:?}: expected the server's own 200, got {status}",
            row.case
        );
        // The program wrote the document with `echo`, which ends the line it wrote: the
        // bytes the server answers with are those bytes.
        assert_eq!(
            body.trim_end(),
            row.body,
            "case {:?}: expected the body {:?}, got {body:?}",
            row.case,
            row.body
        );
        assert_eq!(
            content_type.as_deref(),
            Some("text/html"),
            "case {:?}: expected the document's own media type, got {content_type:?}",
            row.case
        );
        // And a path its document root holds nothing at is answered by the server too, as
        // the one it has no document for.
        let missing = served(&engine, &instance_id, port, "/nothing-here.html");
        assert_eq!(
            missing.map(|(status, _, _)| status),
            Some(404),
            "case {:?}: expected the server's own not-found status for a path it holds \
             nothing at",
            row.case
        );
        // A port the guest is NOT listening on is a connection that cannot be made, whatever
        // it serves on its own.
        let elsewhere = served(&engine, &instance_id, port + 1, "/");
        assert!(
            elsewhere.is_none(),
            "case {:?}: expected nothing on a port no server of the guest listens on, got \
             {elsewhere:?}",
            row.case
        );
    }
}

/// RULE: a program sent to a RUNNING guest changes the same services its boot program does,
/// so a server a later command starts is serving from then on — and the document it serves is
/// whatever the guest's own storage holds at that moment.
#[tokio::test]
async fn a_command_sent_to_a_running_guest_starts_a_server_too() {
    let (router, engine, image_id) = emulator();
    let instance_id = launch(&router, "a later command", &image_id, None).await;
    // The guest has to be up before anything can be run in it.
    let ports = listening_ports(&router, &engine, &instance_id, 0).await;
    assert!(
        ports.is_empty(),
        "expected a guest with no user data to be listening on nothing, got {ports:?}"
    );
    let run = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard.run_in_instance_guest(
            &instance_id,
            &[
                "yum install -y nginx".to_string(),
                "systemctl start nginx".to_string(),
                format!("echo \"{MARKER}\" > /usr/share/nginx/html/index.html"),
            ],
        )
    };
    let run = run.expect("the running guest performs the commands");
    assert_eq!(
        run.status, 0,
        "expected the commands to run to success, got {} and {:?}",
        run.status, run.output
    );
    let answered = served(&engine, &instance_id, 80, "/index.html");
    let Some((status, body, content_type)) = answered else {
        panic!("expected the server the command started to answer, got nothing");
    };
    assert_eq!(status, 200, "expected the server's own 200, got {status}");
    assert_eq!(
        body.trim_end(),
        MARKER,
        "expected the document the command wrote, got {body:?}"
    );
    assert_eq!(
        content_type.as_deref(),
        Some("text/html"),
        "expected the document's own media type, got {content_type:?}"
    );
}

/// RULE: what the guest serves comes out of its OWN storage, so the files the API reports it
/// holding are the files the server answers with — nothing is invented for a path it holds
/// nothing at.
#[tokio::test]
async fn what_is_served_is_what_the_guest_s_storage_holds() {
    let (router, engine, image_id) = emulator();
    let instance_id = launch(
        &router,
        "a document root with two documents",
        &image_id,
        Some(&format!(
            "#!/bin/bash\n\
             yum install -y httpd\n\
             systemctl start httpd\n\
             echo \"{MARKER}\" > /var/www/html/index.html\n\
             echo ok > /var/www/html/health.txt\n"
        )),
    )
    .await;
    listening_ports(&router, &engine, &instance_id, 1).await;
    let held: BTreeMap<String, String> = {
        let store = engine.state::<Ec2State>();
        let guard = store.lock();
        guard
            .instances
            .get(&instance_id)
            .map(|instance| instance.guest_files.clone())
            .unwrap_or_default()
    };
    // (case, the path, the status, the body)
    let rows: &[(&str, &str, u16, &str)] = &[
        ("the health document", "/health.txt", 200, "ok"),
        ("the root document by name", "/index.html", 200, MARKER),
        ("a document the root does not hold", "/missing", 404, ""),
    ];
    for (case, path, status, body) in rows {
        let answered = served(&engine, &instance_id, 80, path);
        assert_eq!(
            answered.as_ref().map(|(status, _, _)| *status),
            Some(*status),
            "case {case:?}: expected status {status}, got {answered:?}"
        );
        assert_eq!(
            answered
                .as_ref()
                .map(|(_, body, _)| body.trim_end().to_string()),
            Some((*body).to_string()),
            "case {case:?}: expected body {body:?}, got {answered:?}"
        );
    }
    assert_eq!(
        held.get("/var/www/html/health.txt")
            .map(|contents| contents.trim_end().to_string()),
        Some("ok".to_string()),
        "expected the guest's own storage to hold the document it serves, got {:?}",
        held.get("/var/www/html/health.txt")
    );
}

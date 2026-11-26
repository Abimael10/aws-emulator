// SPDX-License-Identifier: CC0-1.0

//! Whether what one instance's guest is LISTENING on is KNOWN at all — driven over the wire the
//! service speaks, through the crate's shared [`super::ec2_query`] harness.
//!
//! RULE: **this emulator performs an instance's own user-data program, so the servers it
//! records for that guest are the whole of what the machine listens on exactly when every
//! command of that program was performed here — including the machine given no program at all,
//! which runs nothing and listens on nothing. A program that asked for a command this emulator
//! performs nothing of did not run that command, so whatever it was there to do — a server of
//! the guest's own among it — is held nowhere, and what that machine listens on is NOT known,
//! however many servers this emulator did record for it.**
//!
//! That distinction is what a reader of those ports needs to tell a port nothing answers on
//! from one this emulator simply cannot see: a load balancer's health check of a target
//! registered as this instance fails a check for its port only against ports that are known
//! (`service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10`'s
//! `tests/all/instance_target_health_check_port.rs` drives that end to end).
//!
//! The rows differ along what the rule generalises over: whether the launch carries user data
//! at all, whether every command of that program is one this emulator performs (a
//! distribution's package started through the init system, the interpreter's own HTTP server
//! module) or not (the interpreter handed a program the guest itself wrote, a program this
//! guest has none of, a name that is no program at all), and whether the guest is left
//! listening on a server beside the command that was not performed.

use std::time::{Duration, Instant};

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// How long a row waits for the guest to come up and its program to run, and how often it
/// looks. Generous on purpose — many times the span a launch owes its boot — because the
/// deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// A program every command of which this emulator performs: the package manager installs the
/// distribution's web server, the init system's control program starts its unit, and a
/// redirection writes the document it serves.
const PERFORMED_PROGRAM: &str = "#!/bin/bash\n\
     yum install -y httpd\n\
     systemctl start httpd\n\
     echo ok > /var/www/html/health\n";

/// The port that package's own configuration listens on.
const PACKAGE_PORT: i32 = 80;

/// The interpreter's own HTTP server module, which this emulator performs, on a port of its
/// own.
const PERFORMED_MODULE: &str = "#!/bin/bash\n\
     mkdir -p /srv/site\n\
     python3 -m http.server 8080 --directory /srv/site\n";

/// The port that invocation names.
const MODULE_PORT: i32 = 8080;

/// The interpreter handed a program the GUEST wrote, which this emulator performs nothing of:
/// what that program would have served is held nowhere.
const OWN_PROGRAM: &str = "#!/bin/bash\n\
     mkdir -p /srv/app\n\
     echo \"import http.server\" > /srv/app/server.py\n\
     python3 /srv/app/server.py 8080\n";

/// A server program this guest has none of at all.
const UNKNOWN_PROGRAM: &str = "#!/bin/bash\n\
     socat TCP-LISTEN:8080,fork,reuseaddr EXEC:/srv/app/answer\n";

/// A program that installs and starts a server this emulator performs AND asks for one it does
/// not have: the machine is listening on the first, and what else it listens on is unknown.
const PERFORMED_AND_NOT: &str = "#!/bin/bash\n\
     yum install -y httpd\n\
     systemctl start httpd\n\
     /opt/sidecar --listen 9090\n";

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

/// Look at the instance through the API until its guest has BOOTED — the write that runs its
/// user-data program — and answer whether its listening ports are known, beside the ports this
/// emulator holds for it.
///
/// The look is `DescribeInstances`, so the boot the launch owes is materialised exactly as it
/// is for any other caller; the two facts are read off this service's own store, since nothing
/// of the EC2 API reports what a guest is listening on.
async fn booted(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    instance_id: &str,
) -> (bool, Vec<i32>) {
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
        let read = {
            let store = engine.state::<Ec2State>();
            let guard = store.lock();
            let has_booted = guard
                .instances
                .get(instance_id)
                .is_some_and(|instance| !instance.boots.is_empty());
            has_booted.then(|| {
                (
                    guard.instance_guest_listening_is_known(instance_id),
                    guard.instance_guest_server_ports(instance_id),
                )
            })
        };
        if let Some(read) = read {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the guest of {instance_id} to boot"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row: the user data a launch carries, whether what the guest then listens on is known,
/// and the ports this emulator holds for it.
struct Row {
    case: &'static str,
    user_data: Option<&'static str>,
    known: bool,
    ports: Vec<i32>,
}

/// RULE: what a guest is listening on is known exactly when every command of its own program
/// was performed here — a machine with no program of its own included — and is not known for
/// one that asked for a command this emulator performs nothing of.
#[tokio::test]
async fn a_guests_listening_ports_are_known_only_where_its_whole_program_was_performed() {
    let rows: Vec<Row> = vec![
        Row {
            case: "no user data at all, so the machine runs nothing",
            user_data: None,
            known: true,
            ports: Vec::new(),
        },
        Row {
            case: "a program every command of which this emulator performs",
            user_data: Some(PERFORMED_PROGRAM),
            known: true,
            ports: vec![PACKAGE_PORT],
        },
        Row {
            case: "the interpreter's own HTTP server module on a port of its own",
            user_data: Some(PERFORMED_MODULE),
            known: true,
            ports: vec![MODULE_PORT],
        },
        Row {
            case: "the interpreter handed a program the guest itself wrote",
            user_data: Some(OWN_PROGRAM),
            known: false,
            ports: Vec::new(),
        },
        Row {
            case: "a server program this guest has none of",
            user_data: Some(UNKNOWN_PROGRAM),
            known: false,
            ports: Vec::new(),
        },
        Row {
            case: "a program that starts a server this emulator performs and asks for one it \
                   does not have",
            user_data: Some(PERFORMED_AND_NOT),
            known: false,
            ports: vec![PACKAGE_PORT],
        },
    ];

    let (router, engine, image_id) = emulator();
    for row in &rows {
        let instance = launch(&router, row.case, &image_id, row.user_data).await;
        let (known, ports) = booted(&router, &engine, row.case, &instance).await;
        assert_eq!(
            known, row.known,
            "case {:?}: expected the listening ports to be {}, got {known} with the ports \
             {ports:?}",
            row.case,
            if row.known { "known" } else { "unknown" }
        );
        assert_eq!(
            ports, row.ports,
            "case {:?}: expected this emulator to hold the ports {:?}, got {ports:?}",
            row.case, row.ports
        );
    }
}

/// RULE: an id this store holds no SERVING instance for has no listening ports known either —
/// nothing of what a machine that is not up runs is held anywhere.
#[tokio::test]
async fn an_instance_that_is_not_serving_has_no_listening_ports_known() {
    let (_router, engine, _image_id) = emulator();
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    for (case, instance_id) in [
        ("an id this account holds no instance for", "i-00000000000000000"),
        ("no id at all", ""),
    ] {
        assert!(
            !guard.instance_guest_listening_is_known(instance_id),
            "case {case:?}: expected the listening ports of {instance_id:?} to be unknown"
        );
    }
}

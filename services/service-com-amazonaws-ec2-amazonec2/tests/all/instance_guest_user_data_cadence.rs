// SPDX-License-Identifier: CC0-1.0

//! WHICH BOOTS run an instance's user data, driven over the wire the service actually speaks —
//! `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! RULE: "the user data to make available to the instance" (`RunInstancesRequest$UserData`) is
//! a program the guest's init system RUNS, and the FORM the document declares itself to be —
//! its own header, not anything the request says — decides on which boots it runs: a
//! `#cloud-boothook` script runs on EVERY boot, a `#!` script once per instance, a
//! `#cloud-config`'s `bootcmd` on every boot and its `runcmd` once, and an ARCHIVE (a MIME
//! multipart message) carries one document per part, each run at the cadence its own part
//! declares; a form this emulator runs nothing for runs on no boot at all. What each run
//! writes to the guest's console device is on the console as written, so a restarted guest's
//! console says how many times its program ran.
//!
//! One program is expressed in each of those forms — it counts the boots on the guest's own
//! storage and writes the count to the console — so the rows differ ONLY in the form their
//! document takes (a bare document of each form, and an archive carrying one document per
//! part) and therefore in the cadence it is run at. Every row is read after the launch and
//! after each of two restarts, so a form that runs every boot is told from one that runs once
//! by more than a single observation.

use std::time::Duration;

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, TRANSITION_DELAY};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, described_instance_state, elements, member};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// How many times each row's guest is restarted after its launch, so one boot is never the
/// whole evidence for a cadence.
const RESTARTS: usize = 2;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// Wait until every clock-owed transition started before now has certainly fallen due.
/// Derived from the service's own span, not a literal, so the two cannot drift.
async fn past_the_transition() {
    tokio::time::sleep(TRANSITION_DELAY + Duration::from_millis(200)).await;
}

/// One emulator over its own engine, with one AMI of the calling account seeded the way a
/// register operation would store it — answered beside its image id.
fn emulator() -> (MultiServiceRouter, String) {
    let (router, engine) = super::ec2_query::emulator();
    let image_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard.insert_image(ImageSpec {
            name: "instance-guest-user-data-cadence".to_string(),
            ..ImageSpec::default()
        })
    };
    (router, image_id)
}

/// The program every row runs, as the shell lines it is: it reads the count its earlier boots
/// left on the machine's own storage, adds one, writes it back and reports it on the console
/// device — so the Nth time it runs it reports `<tag> N`.
///
/// Computed from the guest's OWN state rather than printed as a literal, so a row cannot pass
/// on output that was never recomputed.
fn counting_program(tag: &str) -> String {
    let file = format!("/var/lib/{}-runs", tag.to_lowercase());
    format!(
        "COUNT=$(cat {file} 2>/dev/null || echo 0)\n\
         COUNT=$((COUNT + 1))\n\
         echo $COUNT > {file}\n\
         echo \"{tag} $COUNT\" > /dev/console\n"
    )
}

/// The same program as the entry of a `#cloud-config` module: a block scalar, whose lines are
/// indented under it.
fn cloud_config_module(key: &str, tag: &str) -> String {
    let indented: String = counting_program(tag)
        .lines()
        .map(|line| format!("    {line}\n"))
        .collect();
    format!("{key}:\n  - |\n{indented}")
}

/// The same program as one part of a MIME multipart archive, which is no program of its own
/// but a carrier of several: each part declares the form it is to be read as.
fn mime_archive(parts: &[(&str, String)]) -> String {
    let boundary = "==USER-DATA==";
    let mut archive = format!(
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\nMIME-Version: 1.0\n\n"
    );
    for (declared, body) in parts {
        archive.push_str(&format!("--{boundary}\nContent-Type: {declared}\n\n{body}\n"));
    }
    archive.push_str(&format!("--{boundary}--\n"));
    archive
}

/// On WHICH boots the init system runs a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cadence {
    /// Every boot of the guest, so the Nth boot reports the Nth run.
    EveryBoot,
    /// The first boot of the instance only, so every later boot reports nothing.
    FirstBootOnly,
    /// No boot at all: a form this emulator reads no program out of.
    Never,
}

impl Cadence {
    /// The runs a program of this cadence has reported by the time the guest has booted
    /// `boots` times.
    fn reported_by(self, boots: usize) -> Vec<String> {
        let runs = match self {
            Self::EveryBoot => boots,
            Self::FirstBootOnly => 1,
            Self::Never => 0,
        };
        (1..=runs).map(|run| run.to_string()).collect()
    }
}

/// When the init system runs a form's program, and so which lines its console carries.
struct Row {
    case: &'static str,
    /// The document the launch carries as its user data.
    user_data: String,
    /// Each program the document carries: the tag its console lines are written under, and
    /// the cadence that program's own form is run at.
    programs: Vec<(&'static str, Cadence)>,
}

/// The rows the rule is asserted over: one program, one row per form cloud-init documents a
/// cadence for, plus a form this emulator runs nothing for.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // "#cloud-boothook" — the boothook form, run on every boot, written the way one
            // usually is: the header, then the interpreter line of the script under it.
            case: "a boothook, run on every boot",
            user_data: format!(
                "#cloud-boothook\n#!/bin/sh\n{}",
                counting_program("BOOTHOOK")
            ),
            programs: vec![("BOOTHOOK", Cadence::EveryBoot)],
        },
        Row {
            case: "a boothook that carries no interpreter line",
            user_data: format!("#cloud-boothook\n{}", counting_program("BARE-BOOTHOOK")),
            programs: vec![("BARE-BOOTHOOK", Cadence::EveryBoot)],
        },
        Row {
            case: "a user-data script, run once per instance",
            user_data: format!("#!/bin/sh\n{}", counting_program("SCRIPT")),
            programs: vec![("SCRIPT", Cadence::FirstBootOnly)],
        },
        Row {
            case: "a cloud-config carrying both modules",
            user_data: format!(
                "#cloud-config\n{}{}",
                cloud_config_module("bootcmd", "BOOTCMD"),
                cloud_config_module("runcmd", "RUNCMD")
            ),
            programs: vec![
                ("BOOTCMD", Cadence::EveryBoot),
                ("RUNCMD", Cadence::FirstBootOnly),
            ],
        },
        Row {
            // An archive carries a document per part, so the two cadences run side by side on
            // one instance.
            case: "a MIME multipart archive carrying a boothook and a script",
            user_data: mime_archive(&[
                (
                    "text/cloud-boothook",
                    format!("#!/bin/sh\n{}", counting_program("ARCHIVED-BOOTHOOK")),
                ),
                (
                    "text/x-shellscript",
                    format!("#!/bin/sh\n{}", counting_program("ARCHIVED-SCRIPT")),
                ),
            ]),
            programs: vec![
                ("ARCHIVED-BOOTHOOK", Cadence::EveryBoot),
                ("ARCHIVED-SCRIPT", Cadence::FirstBootOnly),
            ],
        },
        Row {
            // The commands are the same ones every other row runs; the header is a form this
            // emulator reads no program out of, so nothing runs them on any boot.
            case: "a form this emulator runs nothing for",
            user_data: format!("#include\n{}", counting_program("INCLUDED")),
            programs: vec![("INCLUDED", Cadence::Never)],
        },
    ]
}

/// The run numbers `tag` reported on `console`, in the order the console carries them.
fn reported_runs(console: &str, tag: &str) -> Vec<String> {
    let marker = format!("{tag} ");
    console
        .lines()
        .filter_map(|line| line.strip_prefix(&marker))
        .map(|count| count.trim().to_string())
        .collect()
}

/// The boots the guest's init system published on the console: one per boot.
fn boots(console: &str) -> usize {
    console
        .lines()
        .filter(|line| line.contains("ci-info: boot-id: "))
        .count()
}

/// Launch one instance carrying `user_data` as its user data (base64, as the API carries it),
/// answering its id.
async fn launch(router: &MultiServiceRouter, case: &str, image_id: &str, user_data: &str) -> String {
    let (status, body) = api(
        router,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("InstanceType", "t3.micro"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p(
                "UserData",
                base64::engine::general_purpose::STANDARD.encode(user_data),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status}: {body}"
    );
    elements(&body, "instanceId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// One instance's console as text — empty for an instance whose guest has written nothing,
/// which is an instance that carries no `output` member at all.
async fn console(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = api(
        router,
        "GetConsoleOutput",
        &[p("InstanceId", instance_id), p("Latest", "true")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetConsoleOutput must succeed, got {status}: {body}"
    );
    let Some(encoded) = member(&body, "output") else {
        return String::new();
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: output must be base64 ({error})"));
    String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("case {case:?}: the console text must be UTF-8: {error}"))
}

/// Queue one reboot of `instance_id`: `RebootInstances` "only queues a request to reboot the
/// specified instances", so the boot it starts falls due a moment later.
async fn reboot(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let (status, body) = api(router, "RebootInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RebootInstances must succeed, got {status}: {body}"
    );
}

/// RULE: the cadence a user-data program runs at is the one its own FORM declares — every
/// boot for a boothook and for a cloud-config `bootcmd`, the first boot only for a `#!` script
/// and for a `runcmd`, and no boot at all for a form nothing reads.
#[tokio::test]
async fn each_user_data_form_runs_on_the_boots_its_own_form_declares() {
    let (router, image_id) = emulator();
    let rows = rows();
    // Every row's instance is launched before anything is read, so the whole table shares the
    // launch's own span rather than waiting out one per row.
    let mut instances: Vec<String> = Vec::new();
    for row in &rows {
        instances.push(launch(&router, row.case, &image_id, &row.user_data).await);
    }

    for restarts in 0..=RESTARTS {
        if restarts > 0 {
            for (row, instance_id) in rows.iter().zip(&instances) {
                reboot(&router, row.case, instance_id).await;
            }
        }
        past_the_transition().await;

        // The guest has booted once per launch plus once per restart, whatever its user data
        // says: the cadence is about the PROGRAM, not about the boots.
        let expected_boots = restarts + 1;
        for (row, instance_id) in rows.iter().zip(&instances) {
            let case = row.case;
            let text = console(&router, case, instance_id).await;
            assert_eq!(
                boots(&text),
                expected_boots,
                "case {case:?}: after {restarts} restart(s) the guest has booted \
                 {expected_boots} time(s), got {text:?}"
            );
            for (tag, cadence) in &row.programs {
                let expected = cadence.reported_by(expected_boots);
                let reported = reported_runs(&text, tag);
                assert_eq!(
                    reported, expected,
                    "case {case:?}: after {restarts} restart(s) a program run {cadence:?} must \
                     have reported the runs {expected:?} under {tag:?}, got {reported:?}"
                );
            }
            // A reboot restarts the operating system rather than the instance.
            assert_eq!(
                described_instance_state(&router, REGION, instance_id).await,
                "running",
                "case {case:?}: a restarted instance is still running"
            );
        }
    }
}

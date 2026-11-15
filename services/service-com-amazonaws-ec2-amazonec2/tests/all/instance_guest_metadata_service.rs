// SPDX-License-Identifier: CC0-1.0

//! The INSTANCE METADATA SERVICE one instance's guest reads itself from, driven over the
//! wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness: a launch, and then the instance's own CONSOLE
//! (`GetConsoleOutput`), which is where what the guest's program read is observable.
//!
//! RULE: **an instance's own facts are published TO its guest at the link-local metadata
//! address, so a program inside the guest reads each of them with the documented
//! `curl <that address>/<category>` and gets the value the API reports for that same
//! instance — including the signed instance identity document an agent registers the machine
//! with, whose signature is a signature OF that document.** AWS documents the read itself:
//! "this document can be found by running the following command from the instance", followed
//! by a `curl` of that address (Amazon ECS's
//! `RegisterContainerInstanceRequest$instanceIdentityDocument`), and the metadata categories
//! below are the ones the Amazon EC2 User Guide publishes under `/latest/meta-data/`.
//!
//! Why a client cares: every agent that runs inside an instance answers "which machine am
//! I" this way — the ECS container agent reads the identity document and its signature
//! before it can register the instance as a container instance at all — so a guest with no
//! metadata service leaves all of them stuck.
//!
//! The rows span what the rule generalises over: WHICH fact is read (the id, the image, the
//! type, the two addresses, the host name, the zone and the zone's Region), the FORM the
//! read takes (an IMDSv1 `GET`, and the IMDSv2 token request followed by a read presenting
//! that token), the MODE the instance's launch chose (a token optional, and a token
//! required), and what is NOT published (a category the service holds nothing at, and any
//! other address — this guest has no network).

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, TRANSITION_DELAY};
use serde_json::Value;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, member};

/// The Region every call here is signed for, which is also the Region the launched
/// instance's zone belongs to.
const REGION: &str = "us-west-2";

/// The prefix each line the boot script writes to the console carries, so a read's answer
/// can be told from everything else the guest wrote.
const MARKER: &str = "IMDS";

/// The metadata service's own address, read from the service rather than written here, with
/// the scheme a documented read names it under.
fn metadata() -> String {
    format!("http://{}", ec2::state::metadata_address())
}

/// `version` the published identity document carries (Amazon EC2 User Guide, "Instance
/// identity documents").
const DOCUMENT_VERSION: &str = "2017-09-30";

/// How many bytes a signature under a 2048-bit key is, which is what the published
/// signature's base64 has to decode to.
const SIGNATURE_BYTES: usize = 256;

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

/// Wait until every clock-owed transition started before now has certainly fallen due —
/// derived from the service's own span rather than a literal.
async fn past_the_transition() {
    tokio::time::sleep(TRANSITION_DELAY + Duration::from_millis(200)).await;
}

/// One emulator over its own engine with one AMI of the calling account seeded the way a
/// register operation would store it, answered beside its image id.
fn emulator() -> (MultiServiceRouter, String) {
    let (router, engine) = super::ec2_query::emulator();
    let image_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard.insert_image(ImageSpec {
            name: "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64".to_string(),
            ..ImageSpec::default()
        })
    };
    (router, image_id)
}

/// The `#cloud-config` user data whose every-boot program runs `commands`.
///
/// Each command is written as a single-quoted YAML scalar, because a command carrying `: `
/// is not a plain scalar: a client writing a header into a boot script quotes it the same
/// way.
fn cloud_config(commands: &[String]) -> String {
    let mut written = String::from("#cloud-config\nbootcmd:\n");
    for command in commands {
        written.push_str("  - '");
        written.push_str(&command.replace('\'', "''"));
        written.push_str("'\n");
    }
    written
}

/// The boot command that reads `path` from the metadata service and writes it to the console
/// under `label`.
fn reads(label: &str, path: &str) -> String {
    format!(
        "echo \"{MARKER} {label} $(curl -s {}{path})\" > /dev/console",
        metadata()
    )
}

/// Launch one instance from `image_id` with `user_data` as its user data, answering its id.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    user_data: &str,
) -> String {
    launch_with(router, case, image_id, user_data, &[]).await
}

/// [`launch`] with `extra` request parameters as well — the metadata options a launch
/// chooses for the instance.
async fn launch_with(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    user_data: &str,
    extra: &[(String, String)],
) -> String {
    let mut params = vec![
        p("ImageId", image_id),
        p("InstanceType", "t3.micro"),
        p("MinCount", "1"),
        p("MaxCount", "1"),
        p(
            "UserData",
            base64::engine::general_purpose::STANDARD.encode(user_data),
        ),
    ];
    params.extend_from_slice(extra);
    let (status, body) = api(router, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status}: {body}"
    );
    elements(&body, "instanceId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// One instance's console as text.
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

/// What the guest wrote under `label`, as the console carries it — `None` for a label no line
/// carries, and `Some("")` for a read that answered nothing.
fn answered(text: &str, label: &str) -> Option<String> {
    let prefix = format!("{MARKER} {label}");
    text.lines()
        .filter_map(|line| line.strip_prefix(prefix.as_str()))
        .map(|rest| rest.trim().to_string())
        .next_back()
}

/// The members `DescribeInstances` reports for `instance_id`, as the harness reads one
/// element of each.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    members: &[&str],
) -> BTreeMap<String, String> {
    let (status, body) = api(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must succeed, got {status}: {body}"
    );
    members
        .iter()
        .map(|name| {
            let value = element(&body, name).unwrap_or_else(|| {
                panic!("case {case:?}: expected a {name} in {body}")
            });
            ((*name).to_string(), value)
        })
        .collect()
}

/// RULE: every published category is the instance's OWN fact — what the guest reads at each
/// path is what the API reports for that same instance.
#[tokio::test]
async fn the_guest_reads_its_own_facts_from_the_metadata_service() {
    // (case, the label the boot script writes it under, the path, the member
    // `DescribeInstances` reports the same fact as)
    let rows: [(&str, &str, &str, &str); 6] = [
        (
            "the instance's own id",
            "instance-id",
            "/latest/meta-data/instance-id",
            "instanceId",
        ),
        (
            "the image it was launched from",
            "ami-id",
            "/latest/meta-data/ami-id",
            "imageId",
        ),
        (
            "its instance type",
            "instance-type",
            "/latest/meta-data/instance-type",
            "instanceType",
        ),
        (
            "its private address",
            "local-ipv4",
            "/latest/meta-data/local-ipv4",
            "privateIpAddress",
        ),
        (
            "the host name the guest answers to",
            "local-hostname",
            "/latest/meta-data/local-hostname",
            "privateDnsName",
        ),
        (
            "the zone it is in",
            "availability-zone",
            "/latest/meta-data/placement/availability-zone",
            "availabilityZone",
        ),
    ];

    let case = "the published categories";
    let mut commands: Vec<String> = rows
        .iter()
        .map(|(_, label, path, _)| reads(label, path))
        .collect();
    // The Region of that zone, which the metadata service publishes as a category of its
    // own, and the LISTING of what it publishes, which answers one category per line.
    commands.push(reads("region", "/latest/meta-data/placement/region"));
    commands.push(format!(
        "curl -s {}/latest/meta-data/ > /dev/console",
        metadata()
    ));

    let (router, image_id) = emulator();
    let instance_id = launch(&router, case, &image_id, &cloud_config(&commands)).await;
    past_the_transition().await;
    let text = console(&router, case, &instance_id).await;
    let reported = described(
        &router,
        case,
        &instance_id,
        &rows.iter().map(|(_, _, _, member)| *member).collect::<Vec<_>>(),
    )
    .await;

    for (row, label, path, member_name) in rows {
        let expected = reported
            .get(member_name)
            .unwrap_or_else(|| panic!("case {row:?}: the API must report {member_name}"));
        assert_eq!(
            answered(&text, label).as_deref(),
            Some(expected.as_str()),
            "case {row:?}: expected the guest's read of {path} to answer the instance's own \
             {member_name} {expected:?}, got {text:?}"
        );
    }
    assert_eq!(
        answered(&text, "region").as_deref(),
        Some(REGION),
        "case \"the Region of its zone\": expected the Region of the instance's own zone, \
         got {text:?}"
    );
    // Every category the rows read is one the LISTING names, so a client can find them.
    for (_, _, path, _) in rows {
        let named = path
            .strip_prefix("/latest/meta-data/")
            .unwrap_or(path)
            .to_string();
        assert!(
            text.lines().any(|line| line.trim() == named),
            "case \"the listing\": expected the listing of the published categories to name \
             {named:?}, got {text:?}"
        );
    }
}

/// RULE: the guest reads a SIGNED identity document of its own machine — every member the
/// instance's record carries, and a signature of that document's bytes.
#[tokio::test]
async fn the_guest_reads_a_signed_identity_document_of_its_own_machine() {
    let case = "the identity document";
    let commands = vec![
        // The documented read, trailing separator included, as
        // `RegisterContainerInstanceRequest$instanceIdentityDocument` spells it.
        format!(
            "curl -s {}/latest/dynamic/instance-identity/document/ > /dev/console",
            metadata()
        ),
        reads(
            "signature",
            "/latest/dynamic/instance-identity/signature",
        ),
    ];
    let (router, image_id) = emulator();
    let instance_id = launch(&router, case, &image_id, &cloud_config(&commands)).await;
    past_the_transition().await;
    let text = console(&router, case, &instance_id).await;
    let reported = described(
        &router,
        case,
        &instance_id,
        &[
            "instanceId",
            "imageId",
            "instanceType",
            "privateIpAddress",
            "availabilityZone",
        ],
    )
    .await;

    let document = text
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("case {case:?}: the guest must read a document: {text:?}"));
    let parsed: Value = serde_json::from_str(document.trim())
        .unwrap_or_else(|error| panic!("case {case:?}: {error}: {document:?}"));
    // (the member of the document, the value it must carry)
    let expected: Vec<(&str, String)> = vec![
        ("instanceId", reported["instanceId"].clone()),
        ("imageId", reported["imageId"].clone()),
        ("instanceType", reported["instanceType"].clone()),
        ("privateIp", reported["privateIpAddress"].clone()),
        ("availabilityZone", reported["availabilityZone"].clone()),
        ("region", REGION.to_string()),
        ("accountId", engine::emulator_account_id().to_string()),
        ("version", DOCUMENT_VERSION.to_string()),
    ];
    for (member_name, value) in expected {
        assert_eq!(
            parsed[member_name].as_str(),
            Some(value.as_str()),
            "case {case:?}: expected {member_name} {value:?} in the document, got {parsed}"
        );
    }
    assert!(
        parsed["pendingTime"]
            .as_str()
            .is_some_and(|written| written.ends_with('Z') && written.contains('T')),
        "case {case:?}: expected the instant the instance was launched, got {parsed}"
    );

    let signature = answered(&text, "signature")
        .unwrap_or_else(|| panic!("case {case:?}: the guest must read a signature: {text:?}"));
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the signature must be base64 ({error}): {signature:?}")
        });
    assert_eq!(
        decoded.len(),
        SIGNATURE_BYTES,
        "case {case:?}: expected a signature of the document's own key size, got {} byte(s)",
        decoded.len()
    );

    // A SECOND instance's document is a different document, so its signature is a different
    // signature: nothing here publishes one constant of the right shape.
    let other = launch(&router, case, &image_id, &cloud_config(&commands)).await;
    past_the_transition().await;
    let other_text = console(&router, case, &other).await;
    let other_signature = answered(&other_text, "signature").unwrap_or_else(|| {
        panic!("case {case:?}: the second guest must read a signature: {other_text:?}")
    });
    assert_ne!(
        other_signature, signature,
        "case {case:?}: two instances' documents must not carry the same signature, got \
         {signature:?} twice"
    );
}

/// RULE: the metadata service answers the IMDSv2 form too — a `PUT` of the token path with a
/// lifetime answers a token, and a read presenting it answers the same fact; an instance
/// whose launch REQUIRES a token answers only a read that presents one; and it answers
/// NOTHING for a category it does not publish, for a token request without a lifetime, or
/// for any other address, because this guest has no network to reach one over.
#[tokio::test]
async fn the_metadata_service_answers_the_token_form_and_nothing_it_does_not_publish() {
    let case = "the token form and what is not published";
    let token_form = vec![
        format!(
            "TOKEN=$(curl -X PUT {}/latest/api/token \
             -H \"X-aws-ec2-metadata-token-ttl-seconds: 21600\")",
            metadata()
        ),
        format!(
            "ID=$(curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
             {}/latest/meta-data/instance-id)",
            metadata()
        ),
        format!("echo \"{MARKER} with-token $ID\" > /dev/console"),
    ];
    let unanswered = vec![
        format!(
            "UNTIMED=$(curl -s -X PUT {}/latest/api/token)",
            metadata()
        ),
        format!("echo \"{MARKER} untimed $UNTIMED\" > /dev/console"),
        reads("unpublished", "/latest/meta-data/not-a-category"),
        format!(
            "echo \"{MARKER} elsewhere $(curl -s http://example.invalid/latest/meta-data/\
             instance-id)\" > /dev/console"
        ),
    ];
    let (router, image_id) = emulator();
    let instance_id = launch(&router, case, &image_id, &cloud_config(&token_form)).await;
    let other = launch(&router, case, &image_id, &cloud_config(&unanswered)).await;
    // An instance whose launch REQUIRES a session token: "if the state is `required`, you
    // must send a session token with any instance metadata retrieval requests"
    // (`InstanceMetadataOptionsResponse$HttpTokens`), so the read without one answers
    // nothing on it and the read presenting one still answers.
    let mut strict_commands = token_form.clone();
    strict_commands.push(reads("without-token", "/latest/meta-data/instance-id"));
    let strict = launch_with(
        &router,
        case,
        &image_id,
        &cloud_config(&strict_commands),
        &[p("MetadataOptions.HttpTokens", "required")],
    )
    .await;
    past_the_transition().await;

    let text = console(&router, case, &instance_id).await;
    assert_eq!(
        answered(&text, "with-token").as_deref(),
        Some(instance_id.as_str()),
        "case {case:?}: expected the read presenting a session token to answer the \
         instance's own id, got {text:?}"
    );

    let required = console(&router, case, &strict).await;
    assert_eq!(
        answered(&required, "with-token").as_deref(),
        Some(strict.as_str()),
        "case \"an instance that requires a session token\": expected the read presenting one \
         to answer the instance's own id, got {required:?}"
    );
    assert_eq!(
        answered(&required, "without-token").as_deref(),
        Some(""),
        "case \"an instance that requires a session token\": expected the read without one to \
         answer nothing, got {required:?}"
    );

    let refused = console(&router, case, &other).await;
    // (the case, the label, the line the guest wrote)
    for (row, label) in [
        ("a token request stating no lifetime", "untimed"),
        ("a category the service does not publish", "unpublished"),
        ("any other address", "elsewhere"),
    ] {
        assert_eq!(
            answered(&refused, label).as_deref(),
            Some(""),
            "case {row:?}: expected the guest's read to answer nothing, got {refused:?}"
        );
    }
}

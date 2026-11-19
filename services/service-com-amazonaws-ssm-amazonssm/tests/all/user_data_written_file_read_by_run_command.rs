// SPDX-License-Identifier: CC0-1.0

//! The flow a client that bootstraps an instance and then reads the result over Systems
//! Manager depends on, end to end over one emulator: the instance writes a file while it
//! boots, becomes a managed node, and a Run Command invocation reads that file back.
//!
//! RULE under test: AN INSTANCE'S GUEST IS ONE MACHINE ACROSS BOTH PLANES — the program its
//! user data gives it runs in the same guest a Run Command invocation runs in, so a
//! `DescribeInstanceInformation` poll reports that instance `Online` once its agent's
//! registration has fallen due, and the invocation's `StandardOutputContent` is what the
//! boot program left on that instance's own storage. The corollary is what makes it an
//! observation rather than an invention: an instance whose user data this emulator runs
//! NOTHING for holds no such file, and the read of it fails the way a shell's own reader
//! fails for a path the machine does not hold.
//!
//! Every call goes through the front door — `AmazonSSM.DescribeInstanceInformation`,
//! `SendCommand` and `GetCommandInvocation` over `awsJson1_1`, the launch over EC2's
//! `ec2Query` wire, and the role, policy and instance profile over IAM's `awsQuery` wire —
//! so each component's state is written and read through the service that owns it. The
//! harness and the instance-profile chain are `ec2_instance_managed_nodes`', the filtered
//! wait for an `Online` node is `command_fan_out_per_node_invocations`', and the send, the
//! wait for a terminal status and the clock seam are `run_command_in_the_guest`'s; this suite
//! adds only what its own axis needs.
//!
//! The rows span what the rule generalises over: the FORM the user data takes (a user-data
//! script, a `#cloud-config` document's `runcmd`, its `write_files` module, and a form
//! nothing is run for), the content it leaves behind (one line, several lines, bytes a shell
//! quotes, a value far longer than a word), and where on the machine it leaves it.
//!
//! Nothing here waits a fixed time: the registration span and the invocation's run are
//! stored due instants, so each case moves the instant through the store the sibling suites
//! move it through and then LOOKS AGAIN until the answer is there or a generous deadline
//! passes.

use serde_json::Value;

use crate::command_fan_out_per_node_invocations::online;
use crate::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, SSM_CORE_POLICY_ARN,
};
// The launch, the filtered wait for an Online node, the send, the wait for a terminal status
// and the clock seams are the sibling managed-node suites': this suite asks the same
// questions of the same state and adds only the user-data axis.
use crate::run_command_in_the_guest::{finish_due, finished, send};

/// The status a shell's reader answers for a path the machine does not hold, which is the
/// status the invocation carrying it reports.
const READ_FAILED: i64 = 1;

/// One row: the user data the launch carries, the path the command reads, and what the guest
/// answers for it.
struct Row {
    case: &'static str,
    /// The text the guest is given as its user data, or `None` for a launch that carries
    /// none at all.
    user_data: Option<String>,
    /// The path the Run Command invocation reads.
    path: &'static str,
    /// The content the boot program left at that path, or `None` when the guest holds no
    /// file there.
    content: Option<String>,
}

/// The rows of the table, built once so the long-value case can name its own size.
fn rows() -> Vec<Row> {
    // The size the long-value row writes: far longer than one word and well inside the
    // ceiling on one command of a boot program
    // (`service_com_amazonaws_ec2_amazonec2::state::guest_user_data::MAX_GUEST_COMMAND_BYTES`).
    let long = "v".repeat(512);
    vec![
        Row {
            case: "a user-data script writing one line",
            user_data: Some(
                "#!/bin/bash\necho s3cr3t-value > /tmp/decrypted-secret.txt\n".to_string(),
            ),
            path: "/tmp/decrypted-secret.txt",
            content: Some("s3cr3t-value\n".to_string()),
        },
        Row {
            case: "a user-data script writing bytes a shell quotes",
            user_data: Some(
                "#!/bin/bash\necho 'alpha,beta;gamma=1' > /tmp/punctuated.txt\n".to_string(),
            ),
            path: "/tmp/punctuated.txt",
            content: Some("alpha,beta;gamma=1\n".to_string()),
        },
        Row {
            case: "a user-data script appending a second line",
            user_data: Some(
                "#!/bin/bash\necho first > /var/tmp/appended.txt\necho second >> \
                 /var/tmp/appended.txt\n"
                    .to_string(),
            ),
            path: "/var/tmp/appended.txt",
            content: Some("first\nsecond\n".to_string()),
        },
        Row {
            case: "a user-data script making a directory and writing under it",
            user_data: Some(
                "#!/bin/bash\nmkdir -p /opt/app\necho nested > /opt/app/secret.txt\n".to_string(),
            ),
            path: "/opt/app/secret.txt",
            content: Some("nested\n".to_string()),
        },
        Row {
            case: "a user-data script writing a value far longer than a word",
            user_data: Some(format!("#!/bin/bash\necho {long} > /tmp/long.txt\n")),
            path: "/tmp/long.txt",
            content: Some(format!("{long}\n")),
        },
        Row {
            case: "a cloud-config document whose runcmd writes the file",
            user_data: Some(
                "#cloud-config\nruncmd:\n  - echo from-runcmd > /tmp/runcmd-secret.txt\n"
                    .to_string(),
            ),
            path: "/tmp/runcmd-secret.txt",
            content: Some("from-runcmd\n".to_string()),
        },
        Row {
            case: "a cloud-config document whose write_files names the file",
            user_data: Some(
                concat!(
                    "#cloud-config\n",
                    "write_files:\n",
                    "  - path: /etc/app/secret.conf\n",
                    "    content: |\n",
                    "      token=abc123\n",
                    "      region=us-west-2\n",
                )
                .to_string(),
            ),
            path: "/etc/app/secret.conf",
            content: Some("token=abc123\nregion=us-west-2\n".to_string()),
        },
        Row {
            case: "a launch carrying no user data at all",
            user_data: None,
            path: "/tmp/decrypted-secret.txt",
            content: None,
        },
        Row {
            case: "user data in a form this emulator runs nothing for",
            user_data: Some("#include\nhttps://example.invalid/script\n".to_string()),
            path: "/tmp/decrypted-secret.txt",
            content: None,
        },
    ]
}

/// Launch one instance running as `profile`, carrying `user_data` when the row has one, and
/// answer the instance id Amazon EC2 minted.
///
/// The launch instant is then moved back past the registration span, so the case waits on an
/// answer rather than on a clock.
async fn launch_with_user_data(
    emulator: &Emulator,
    case: &str,
    image: &str,
    profile: &str,
    user_data: Option<&str>,
) -> String {
    let instance = emulator
        .launch_with_user_data(case, image, Some(profile), user_data)
        .await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    instance
}

/// Read `path` out of `instance`'s guest through Run Command and answer the finished
/// invocation.
///
/// The instant the invocation's run was owed is moved to now, so the case does not wait out
/// that span before it looks at what the guest answered.
async fn read_file(emulator: &Emulator, case: &str, instance: &str, path: &str) -> Value {
    let command_id = send(emulator, case, instance, &[&format!("cat {path}")]).await;
    finish_due(emulator, &command_id);
    finished(emulator, case, &command_id, instance).await
}

/// RULE: the guest a boot program ran in is the guest a Run Command invocation runs in, so
/// what the instance's user data wrote is what the invocation reads back — and an instance
/// whose user data ran nothing holds no such file.
#[tokio::test]
async fn an_invocation_reads_back_what_the_instances_user_data_wrote() {
    let emulator = Emulator::new();
    let image = emulator.image("the user-data table", AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(
            "the user-data table",
            "user-data-role",
            &Grant::Attached(SSM_CORE_POLICY_ARN),
        )
        .await;

    // Every row's instance is launched first, so each one's registration and each one's boot
    // are owed before any of them is asked about.
    let rows = rows();
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        let instance = launch_with_user_data(
            &emulator,
            row.case,
            &image,
            &profile,
            row.user_data.as_deref(),
        )
        .await;
        launched.push((row, instance));
    }

    for (row, instance) in &launched {
        let case = row.case;
        let node = online(&emulator, case, instance).await;
        assert_eq!(
            node["ResourceType"].as_str(),
            Some("EC2Instance"),
            "case {case:?}: the node reported is the EC2 instance that registered, got {node}"
        );

        let answered = read_file(&emulator, case, instance, row.path).await;
        match &row.content {
            Some(content) => {
                assert_eq!(
                    answered["Status"].as_str(),
                    Some("Success"),
                    "case {case:?}: reading a file the boot program wrote succeeds, got \
                     {answered}"
                );
                assert_eq!(
                    answered["ResponseCode"].as_i64(),
                    Some(0),
                    "case {case:?}: the reader exits cleanly, got {answered}"
                );
                assert_eq!(
                    answered["StandardOutputContent"].as_str(),
                    Some(content.as_str()),
                    "case {case:?}: expected the bytes the user data left at {}, got {answered}",
                    row.path
                );
            }
            None => {
                // The guest's session reports ONE stream, so the reader's diagnostic is what
                // the invocation carries as its output rather than a separate error stream
                // (the sibling in-guest suite's `ls` rows take the same reading).
                assert_eq!(
                    answered["Status"].as_str(),
                    Some("Failed"),
                    "case {case:?}: reading a file no boot program wrote must fail, got \
                     {answered}"
                );
                assert_eq!(
                    answered["ResponseCode"].as_i64(),
                    Some(READ_FAILED),
                    "case {case:?}: expected the status a reader answers for a path the \
                     machine does not hold, got {answered}"
                );
                assert_eq!(
                    answered["StandardOutputContent"].as_str(),
                    Some(
                        format!("cat: {}: No such file or directory\n", row.path).as_str()
                    ),
                    "case {case:?}: expected the reader's own diagnostic, got {answered}"
                );
            }
        }
    }
}

/// RULE: reading the machine through Run Command is a READ — so the file the user data wrote
/// is still there, byte for byte, for the next invocation, and the node stays reported
/// `Online` across both.
#[tokio::test]
async fn what_the_user_data_wrote_is_still_there_for_a_later_invocation() {
    let case = "the repeated read case";
    let emulator = Emulator::new();
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "repeated-read-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = launch_with_user_data(
        &emulator,
        case,
        &image,
        &profile,
        Some("#!/bin/bash\necho kept-across-reads > /tmp/kept.txt\n"),
    )
    .await;

    online(&emulator, case, &instance).await;
    for read in 1..=2 {
        let answered = read_file(&emulator, case, &instance, "/tmp/kept.txt").await;
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some("kept-across-reads\n"),
            "case {case:?}: read {read} must report the bytes the boot program wrote, got \
             {answered}"
        );
    }
    let node = online(&emulator, case, &instance).await;
    assert_eq!(
        node["PingStatus"].as_str(),
        Some("Online"),
        "case {case:?}: the node is still pinging after its guest was read, got {node}"
    );
}

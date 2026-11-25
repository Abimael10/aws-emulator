// SPDX-License-Identifier: CC0-1.0

//! What a deployment of a source bundle leaves ON the environment's instance,
//! over the service's own HTTP surface.
//!
//! The rule under test: when the bundle a version carries holds
//! `.ebextensions/*.config` files, the deployment APPLIES them on the instance
//! before the web process is started — every `files:` entry is written to the path
//! it declares with exactly the content it declares, every `commands:` entry runs
//! before the application is set up and every `container_commands:` entry runs
//! after it is extracted and before that process starts, in the order their keys
//! put them in — while a path no section of those files mentions does not exist, so
//! a read of it fails.
//!
//! `running_application_process.rs` pins that rule for one entry of each applied
//! section; this suite spans what it generalises over, which is what a
//! configuration file may vary:
//!
//! * the MEMBERS a `files:` entry states beside its content — the mode, the owner
//!   and the group Elastic Beanstalk's own configuration-file reference documents
//!   for that section;
//! * the SHAPE of the content — one line, several lines, and a path whose parent
//!   directory the deployment has to create;
//! * the ORDER applying them follows — the commands of one section in the
//!   alphabetical order of their keys, `commands:` before `container_commands:`,
//!   and one configuration file of the folder after another by name, so the entry
//!   applied last is the one whose content a declared path holds;
//! * what a command entry states BESIDE its command line, the directory it runs in;
//! * and the negative control, a path no section mentions.
//!
//! Every assertion is driven through what the deployed APPLICATION answers when it
//! reads that path off its own instance, so what is under test is the deployment's
//! own work rather than anything this emulator could have written itself: the probe
//! server, the bundle upload, the launch and the unsigned GET are the ones
//! `running_application_process.rs` owns in this same test binary.

use crate::running_application_process::{answered, launched, published};

/// The first configuration file of the bundle's `.ebextensions` folder, by name:
/// the `files:` entry whose members state the content, the mode, the owner and the
/// group, a path whose parent directory the deployment has to create, a path a
/// LATER file declares again, and the command that runs before the application is
/// set up.
const FILES_CONFIG: &[u8] = br#"
files:
  "/tmp/eb-bundled/mapping.txt":
    mode: "000644"
    owner: root
    group: root
    content: |
      first line
      second line
  "/tmp/eb-bundled/nested/deeper/under.txt":
    content: written under a directory the deployment created
  "/tmp/eb-bundled/applied-last.txt":
    content: from the first configuration file
commands:
  00_before_the_application_is_set_up:
    command: printf before > /tmp/eb-bundled/before.txt
"#;

/// The second configuration file of that folder: an entry whose content is one
/// line, the entry that declares the shared path again, and the container commands
/// — two whose keys order them, one that reads what the earlier section's command
/// left behind, and one that reports the directory it was told to run in.
const COMMANDS_CONFIG: &[u8] = br#"
files:
  "/tmp/eb-bundled/single-line.txt":
    content: declared on one line
  "/tmp/eb-bundled/applied-last.txt":
    content: from the second configuration file
container_commands:
  02_append_second:
    command: printf second >> /tmp/eb-bundled/order.txt
  01_write_first:
    command: printf first > /tmp/eb-bundled/order.txt
  03_read_what_ran_before:
    command: cat /tmp/eb-bundled/before.txt > /tmp/eb-bundled/after.txt
  04_report_the_directory:
    command: pwd > /tmp/eb-bundled/where.txt
    cwd: /tmp/eb-bundled/work
"#;

/// One path of the instance the probe is asked to read, and what the deployment
/// left there.
struct ReadRow {
    case: &'static str,
    /// The path on the instance.
    path: &'static str,
    /// The status the probe answers for it: 200 for a path the deployment wrote,
    /// and the probe's own 404 for one nothing did.
    status: u16,
    /// The body the probe answers with, which for a path that exists is its
    /// content, byte for byte.
    body: &'static str,
}

/// One row per thing the rule generalises over.
const READ_ROWS: &[ReadRow] = &[
    ReadRow {
        case: "a files: entry whose members state the content, the mode and the owner",
        path: "/tmp/eb-bundled/mapping.txt",
        status: 200,
        // The block the entry declares, both of its lines, as it declares them.
        body: "first line\nsecond line\n",
    },
    ReadRow {
        case: "a files: entry whose content is one line",
        path: "/tmp/eb-bundled/single-line.txt",
        status: 200,
        body: "declared on one line",
    },
    ReadRow {
        case: "a files: entry under a directory the deployment has to create",
        path: "/tmp/eb-bundled/nested/deeper/under.txt",
        status: 200,
        body: "written under a directory the deployment created",
    },
    ReadRow {
        case: "a path two configuration files of the folder declare",
        path: "/tmp/eb-bundled/applied-last.txt",
        // The files are applied in the order of their names, so the entry of the
        // file applied LAST is the content the path holds.
        status: 200,
        body: "from the second configuration file",
    },
    ReadRow {
        case: "the container commands in the order their keys put them in",
        path: "/tmp/eb-bundled/order.txt",
        status: 200,
        // The first command wrote, the second appended: the other order would
        // leave only what the first one wrote.
        body: "firstsecond",
    },
    ReadRow {
        case: "a container command reading what the commands: section left behind",
        path: "/tmp/eb-bundled/after.txt",
        status: 200,
        body: "before",
    },
    ReadRow {
        case: "the directory a command entry declares it runs in",
        path: "/tmp/eb-bundled/where.txt",
        status: 200,
        body: "/tmp/eb-bundled/work\n",
    },
    ReadRow {
        case: "a path no section of either configuration file mentions",
        path: "/tmp/eb-bundled/never-written.txt",
        status: 404,
        body: "no such file",
    },
];

/// RULE: a deployment of a bundle carrying `.ebextensions/*.config` applies those
/// files on the instance before the web process is started — each `files:` entry is
/// written to its declared path with exactly its declared content, whatever its
/// other members state, and each `commands:` and `container_commands:` entry really
/// runs, in the order the sections and the entries' own keys put them in — while a
/// path no section mentions does not exist on that instance.
#[tokio::test]
async fn the_deployment_applies_every_section_of_the_bundles_configuration() {
    let case = "a bundle whose configuration files write and run";
    let emulator = crate::lifecycle::Emulator::with_object_storage();
    published(
        &emulator,
        case,
        "v1",
        &[
            (".ebextensions/01-files.config", FILES_CONFIG),
            (".ebextensions/02-commands.config", COMMANDS_CONFIG),
        ],
    )
    .await;
    launched(&emulator, case, "provisioned-env", "v1", &[]).await;

    for row in READ_ROWS {
        let case = row.case;
        let path = row.path;
        let read = answered(&emulator, case, "provisioned-env", &format!("/file?{path}")).await;
        assert_eq!(
            read.status, row.status,
            "case {case:?}: expected {} for {path}, got {} and body {:?}",
            row.status, read.status, read.body
        );
        assert_eq!(
            read.body, row.body,
            "case {case:?}: expected {:?} at {path}, got {:?}",
            row.body, read.body
        );
    }
}

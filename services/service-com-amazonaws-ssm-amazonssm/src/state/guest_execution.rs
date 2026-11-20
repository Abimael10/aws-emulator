// SPDX-License-Identifier: CC0-1.0

//! Running a command document's own script IN THE GUEST of the node it was sent to.
//!
//! # What a Run Command invocation is
//!
//! "Run Command … lets you remotely and securely manage … run commands on your managed
//! nodes" — a command's invocation is the document's script PERFORMED on the node, and what
//! the client reads back is what that script wrote and the exit code it ended on
//! (`GetCommandInvocation`'s `StandardOutputContent` and `ResponseCode`). A send that only
//! recorded the request would therefore implement nothing: the invocation would never
//! finish, and a client polling for a terminal status would poll for ever.
//!
//! # Where it runs
//!
//! The agent that runs it is INSIDE the guest, and the guest belongs to the service that
//! owns instances: its storage, its host name and the identity the kernel published to it
//! are facts of the EC2 instance. So this module asks that service to perform the script in
//! that machine, through its own seam
//! (`ComAmazonawsEc2Amazonec2State::run_in_instance_guest`, which states what a run is and
//! what it is not) — the same bounded, in-process shell session the instance's own boot
//! script runs in. There is no process, no socket and no host path in it, a command the
//! session provides no program for answers the status a shell answers for a utility it could
//! not find, and nothing is reported for a node whose guest is not up.
//!
//! # Which script
//!
//! The script is the one the DOCUMENT declares for the node's platform: the step whose
//! plugin runs on it ([`super::document_platforms::step_runs_on`]), with the document's
//! `commands` input resolved from the parameters the send supplied and the defaults the
//! document declares. A document that declares no step this node runs — a Windows document
//! sent to a Linux node — is not something the node's agent performs, and the send's own
//! `UnsupportedPlatformType` refusal is what a client is told about that.

use std::collections::BTreeMap;
use std::time::SystemTime;

use ::service_com_amazonaws_ec2_amazonec2::state as ec2_store;

use crate::ops::types::DocumentParameter;

use super::document_references::whole_reference;
use super::{load_single_document, mapping_get, scalar_string};

/// The input of a command document's step that carries the script it runs: "commands" is
/// the input `aws:runShellScript` and `aws:runPowerShellScript` both take the script in
/// (the Systems Manager command document plugin reference).
pub const COMMANDS_INPUT: &str = "commands";

/// The step of `content` a node of `platform_type` runs, with its script resolved: the
/// step's name and the lines the guest performs.
#[derive(Debug, Clone)]
pub struct ShellStep {
    /// The step's own `name`, which is what `GetCommandInvocation`'s `PluginName` names.
    pub name: String,
    /// The lines of the script, in the document's own order.
    pub commands: Vec<String>,
}

/// The step of the document `content` that a node of `platform_type` runs, or `None` when
/// the content declares no step with a script that node runs.
///
/// `supplied` are the parameter values the send carried and `declared` the document's own
/// parameters, whose `DefaultValue` fills in for one the send left out — "If you don't
/// specify a value, the default value is used" is what a document's default is for.
pub fn shell_step(
    content: &str,
    platform_type: &str,
    supplied: &BTreeMap<String, Vec<String>>,
    declared: &[DocumentParameter],
) -> Option<ShellStep> {
    let document = load_single_document(content).ok()?;
    let map = document.as_hash()?;
    let steps = mapping_get(map, "mainSteps")?.as_vec()?;
    let values = parameter_values(supplied, declared);
    for step in steps {
        let step = step.as_hash()?;
        if !super::document_platforms::step_runs_on(step, platform_type) {
            continue;
        }
        let inputs = mapping_get(step, "inputs").and_then(::yaml_rust2::Yaml::as_hash);
        let Some(script) = inputs.and_then(|inputs| mapping_get(inputs, COMMANDS_INPUT)) else {
            // A step of this document that takes no script is one with nothing for the
            // guest's shell to perform; another step of the same document may still have
            // one.
            continue;
        };
        let commands = resolved_script(script, &values, 0);
        if commands.is_empty() {
            continue;
        }
        return Some(ShellStep {
            name: mapping_get(step, "name")
                .and_then(scalar_string)
                .unwrap_or_default(),
            commands,
        });
    }
    None
}

/// The value of every parameter the document has one for: the document's declared default
/// first, then whatever the send supplied for it.
fn parameter_values(
    supplied: &BTreeMap<String, Vec<String>>,
    declared: &[DocumentParameter],
) -> BTreeMap<String, Vec<String>> {
    let mut values: BTreeMap<String, Vec<String>> = declared
        .iter()
        .filter_map(|parameter| {
            let name = parameter.name.clone()?;
            let default = parameter.default_value.clone()?;
            Some((name, vec![default]))
        })
        .collect();
    for (name, value) in supplied {
        values.insert(name.clone(), value.clone());
    }
    values
}

/// How deeply a step's `commands` input is walked: the input is a list of lines, so one
/// level of nesting is all a document needs, and this leaves room for a document that nests
/// a list inside one.
///
/// The content is a caller's text and the walk recurses once per level, so the depth it can
/// ask for is bounded here: a stack this emulator cannot unwind from is not a refusal, it is
/// the process gone.
const MAX_SCRIPT_DEPTH: usize = 8;

/// How many lines of script one step yields, however many the document and its parameters
/// describe between them.
const MAX_SCRIPT_LINES: usize = 256;

/// How many bytes ONE line of script comes to once its parameter references are resolved.
const MAX_SCRIPT_LINE_BYTES: usize = 8 * 1024;

/// The lines of the script a step's `commands` input holds, with the document's parameter
/// references resolved.
///
/// The input is written either as ONE reference to a `StringList` parameter
/// (`"commands": "{{ commands }}"`, which is how the published command documents take a
/// caller's script) or as a list of lines, each of which may carry references of its own.
/// A reference that IS the whole value resolves to the list it names, line for line; one
/// inside a line resolves to its value in that line.
///
/// Bounded in all three directions a caller chooses: how deep the content nests
/// ([`MAX_SCRIPT_DEPTH`]), how many lines it comes to ([`MAX_SCRIPT_LINES`]) and how long
/// one resolved line is ([`MAX_SCRIPT_LINE_BYTES`]).
fn resolved_script(
    script: &::yaml_rust2::Yaml,
    values: &BTreeMap<String, Vec<String>>,
    depth: usize,
) -> Vec<String> {
    if depth >= MAX_SCRIPT_DEPTH {
        return Vec::new();
    }
    match script {
        ::yaml_rust2::Yaml::Array(lines) => {
            let mut resolved: Vec<String> = Vec::new();
            for line in lines {
                if resolved.len() >= MAX_SCRIPT_LINES {
                    break;
                }
                resolved.extend(resolved_script(line, values, depth + 1));
            }
            resolved.truncate(MAX_SCRIPT_LINES);
            resolved
        }
        other => {
            let Some(text) = scalar_string(other) else {
                return Vec::new();
            };
            let mut lines = match whole_reference(&text).and_then(|name| values.get(name)) {
                Some(named) => named.clone(),
                None => vec![resolved_line(&text, values)],
            };
            lines.truncate(MAX_SCRIPT_LINES);
            lines
        }
    }
}

/// One line of script with its `{{ name }}` references resolved — a list parameter joined
/// by commas, which is how a reference to one inside a line reads.
///
/// The scan and the substitution are the shared ones every document of this service reads
/// its references through ([`super::document_references`]), measured before they are built
/// and held to [`MAX_SCRIPT_LINE_BYTES`]. A reference to a parameter the send carried no
/// value for is left as the document wrote it: the document asked for something the request
/// did not carry, which the send's own parameter validation is what refuses. A line whose
/// resolution would run past the ceiling resolves to NOTHING, so the guest is handed no such
/// line rather than a truncated one.
fn resolved_line(text: &str, values: &BTreeMap<String, Vec<String>>) -> String {
    let value_of = |name: &str| {
        Some(
            values
                .get(name)
                .map(|value| value.join(","))
                // The reference as written, which the scan does not look at again.
                .unwrap_or_else(|| format!("{{{{ {name} }}}}")),
        )
    };
    let mut budget = MAX_SCRIPT_LINE_BYTES;
    super::document_references::resolved_text(text, &value_of, &mut budget).unwrap_or_default()
}

/// Perform `commands` in the guest of the EC2 instance `instance_id` names, answering what
/// the guest wrote and the status it ended on — or `None` when this engine holds no EC2
/// store, or that store holds no running, booted guest for the id.
///
/// The boot the guest was already owed is materialised first: an instance that has just
/// started has its operating system coming up at its own instant, and a script runs in the
/// guest that boot brought up.
///
/// The session is given the resolver clients of the guest besides
/// ([`super::guest_resolver`]): a script that resolves a name is answered by the DNS of the
/// VPC the node is in, which is read ONCE here — before the store that owns the guest is
/// taken — because answering a query reads three stores and running the script holds one.
///
/// The machine's own IDENTITY travels the same way: which role the instance's instance
/// profile carries is a fact IAM holds, so that service's own records answer it
/// ([`ec2_store::instance_profile_role_name`], read before the guest's store is taken), and
/// the guest's metadata service publishes it to a program that asks — the read an agent makes
/// to learn the role it is running as.
pub fn perform_in_guest<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
    commands: &[String],
    now: SystemTime,
) -> Option<ec2_store::GuestRun> {
    let _ = now;
    let resolver = ::service_com_amazonaws_route53resolver_route53resolver::state::vpc_dns::view_from_instance(
        engine, instance_id,
    )
    .map(super::guest_resolver::resolver_programs);
    let identity = ec2_store::GuestIdentity {
        iam_role_name: ec2_store::instance_profile_role_name(engine, instance_id),
    };
    let ec2 = ec2_store::service_state(engine)?;
    let mut store = ec2.lock();
    // The moves that service already owed the instance — its launch reaching `running` and
    // its guest's first boot — were due at their own instants whether or not anybody called,
    // and a script runs in the guest that boot brought up. Materialising them is EC2's own
    // write, through its own one-stop materialiser.
    store.materialize_due_transitions();
    store.run_in_instance_guest_with(instance_id, commands, resolver, &identity)
}

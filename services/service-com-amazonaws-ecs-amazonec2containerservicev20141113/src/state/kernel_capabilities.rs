// SPDX-License-Identifier: CC0-1.0

//! The Linux KERNEL CAPABILITIES a container definition adds to or drops from the set Docker
//! gives it, and the names those two members take.
//!
//! `KernelCapabilities$add` and `$drop` each publish the same closed catalog in their own docs
//! — "Valid values: `"ALL" | "AUDIT_CONTROL" | "AUDIT_WRITE" | ... | "WAKE_ALARM"`"
//! (`smithy_query(query="shape",shape="KernelCapabilities")`) — and the model targets a plain
//! `StringList` for both, so the generated request walk has nothing to hold a value to. The
//! names are the `capabilities(7)` ones WITHOUT the kernel's `CAP_` prefix, which is the
//! spelling the Docker `--cap-add`/`--cap-drop` options those members map to take; a name
//! outside the catalog names no capability Amazon ECS could give or take away.
//!
//! Read for every registration that carries container definitions through the view that
//! weighs a container's whole `linuxParameters` ([`super::linux_parameters`]).

use crate::ops::types::KernelCapabilities;

/// Every Linux capability name `KernelCapabilities$add` and `$drop` accept, as those members
/// publish them: `ALL`, which stands for the whole default set, and the 37 individual
/// capabilities, in the order the docs list them.
pub const KERNEL_CAPABILITY_VALUES: [&str; 38] = [
    "ALL",
    "AUDIT_CONTROL",
    "AUDIT_WRITE",
    "BLOCK_SUSPEND",
    "CHOWN",
    "DAC_OVERRIDE",
    "DAC_READ_SEARCH",
    "FOWNER",
    "FSETID",
    "IPC_LOCK",
    "IPC_OWNER",
    "KILL",
    "LEASE",
    "LINUX_IMMUTABLE",
    "MAC_ADMIN",
    "MAC_OVERRIDE",
    "MKNOD",
    "NET_ADMIN",
    "NET_BIND_SERVICE",
    "NET_BROADCAST",
    "NET_RAW",
    "SETFCAP",
    "SETGID",
    "SETPCAP",
    "SETUID",
    "SYS_ADMIN",
    "SYS_BOOT",
    "SYS_CHROOT",
    "SYS_MODULE",
    "SYS_NICE",
    "SYS_PACCT",
    "SYS_PTRACE",
    "SYS_RAWIO",
    "SYS_RESOURCE",
    "SYS_TIME",
    "SYS_TTY_CONFIG",
    "SYSLOG",
    "WAKE_ALARM",
];

/// The member that ADDS capabilities to the default set, as the API spells it.
pub const CAPABILITIES_ADD_MEMBER: &str = "add";

/// The member that REMOVES them from it.
pub const CAPABILITIES_DROP_MEMBER: &str = "drop";

/// `ClientException$message` for capability names that name no Linux capability, reported
/// against the member they came in on.
///
/// Amazon ECS states the member and then the names it did not recognise, in the order the
/// request listed them — the only values a caller has to change — and nothing else of the
/// request. RECORDED behaviour.
pub fn unrecognized_capabilities_message(member: &str, rejected: &[&str]) -> String {
    format!(
        "Unrecognized Linux capabilities in {member}: [{}]",
        rejected.join(", ")
    )
}

/// The first of a container's two capability lists that names a capability outside the
/// catalog, stated as the refusal's message — or `None` when every name in both is one
/// [`KERNEL_CAPABILITY_VALUES`] holds (and for a container that configures none).
///
/// The members are examined in the order the shape declares them, `add` before `drop`, and
/// the message carries EVERY unrecognised name of the member at fault rather than only the
/// first: they are all names the caller has to change.
pub fn kernel_capabilities_violation(capabilities: Option<&KernelCapabilities>) -> Option<String> {
    let capabilities = capabilities?;
    for (member, names) in [
        (CAPABILITIES_ADD_MEMBER, capabilities.add.as_deref()),
        (CAPABILITIES_DROP_MEMBER, capabilities.drop.as_deref()),
    ] {
        let rejected: Vec<&str> = names
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .filter(|name| !KERNEL_CAPABILITY_VALUES.contains(name))
            .collect();
        if !rejected.is_empty() {
            return Some(unrecognized_capabilities_message(member, &rejected));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `capabilities` configuration naming `add` and `drop` as given.
    fn capabilities(add: &[&str], drop: &[&str]) -> KernelCapabilities {
        KernelCapabilities {
            add: Some(add.iter().map(|name| (*name).to_string()).collect()),
            drop: Some(drop.iter().map(|name| (*name).to_string()).collect()),
        }
    }

    /// RULE: a name the catalog holds is accepted, in either member; one it does not is
    /// refused against the member it came in on.
    #[test]
    fn a_capability_outside_the_catalog_is_refused_against_its_member() {
        // A catalog entry the issue this rule was found on did not cite, read off the
        // member's own doc ([`KERNEL_CAPABILITY_VALUES`]).
        let accepted = capabilities(&["NET_BIND_SERVICE"], &["MKNOD", "ALL"]);
        let answer = kernel_capabilities_violation(Some(&accepted));
        assert!(
            answer.is_none(),
            "case \"catalog names\": expected the configuration to be accepted, got {answer:?}"
        );

        // (case, add, drop, the sentence the refusal carries)
        let rows: &[(&str, &[&str], &[&str], &str)] = &[
            (
                "a CAP_-prefixed name in add",
                &["CAP_SYS_ADMIN"],
                &[],
                "Unrecognized Linux capabilities in add: [CAP_SYS_ADMIN]",
            ),
            (
                "a CAP_-prefixed name in drop",
                &[],
                &["CAP_MKNOD"],
                "Unrecognized Linux capabilities in drop: [CAP_MKNOD]",
            ),
            (
                "a lower-case name",
                &["sys_ptrace"],
                &[],
                "Unrecognized Linux capabilities in add: [sys_ptrace]",
            ),
            (
                "two unrecognised names in one member",
                &["CAP_CHOWN", "NOT_A_CAPABILITY"],
                &[],
                "Unrecognized Linux capabilities in add: [CAP_CHOWN, NOT_A_CAPABILITY]",
            ),
            (
                "a recognised name beside an unrecognised one",
                &["CHOWN", "CAP_KILL"],
                &[],
                "Unrecognized Linux capabilities in add: [CAP_KILL]",
            ),
            (
                "add recognised and drop not",
                &["CHOWN"],
                &["CAP_KILL"],
                "Unrecognized Linux capabilities in drop: [CAP_KILL]",
            ),
        ];
        for (case, add, drop, message) in rows {
            let configured = capabilities(add, drop);
            let answer = kernel_capabilities_violation(Some(&configured));
            assert_eq!(
                answer.as_deref(),
                Some(*message),
                "case {case:?}: expected {message:?}, got {answer:?}"
            );
        }
    }

    /// RULE: a container that configures no capabilities at all breaks nothing.
    #[test]
    fn an_unconfigured_container_is_accepted() {
        for (case, configured) in [
            ("no capabilities member", None),
            (
                "both lists absent",
                Some(KernelCapabilities {
                    add: None,
                    drop: None,
                }),
            ),
            ("both lists empty", Some(capabilities(&[], &[]))),
        ] {
            let answer = kernel_capabilities_violation(configured.as_ref());
            assert!(
                answer.is_none(),
                "case {case:?}: expected the configuration to be accepted, got {answer:?}"
            );
        }
    }
}

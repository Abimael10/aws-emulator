// SPDX-License-Identifier: CC0-1.0

//! The MOUNT OPTIONS of a tmpfs volume a container definition mounts.
//!
//! `Tmpfs$mountOptions` is "The list of tmpfs volume mount options" and publishes the closed
//! catalog it takes in its own doc — "Valid values: `"defaults" | "ro" | "rw" | ... | "mpol"`"
//! (`smithy_query(query="doc",shape="Tmpfs",member="mountOptions")`) — while the model targets
//! a plain `StringList`, so the generated request walk has nothing to hold a value to. The
//! options are the `mount(8)` ones, lower case, and a value outside the catalog names no
//! option Amazon ECS could mount the volume with.
//!
//! Read for every registration that carries container definitions through the view that
//! weighs a container's whole `linuxParameters` ([`super::linux_parameters`]).

use crate::ops::types::Tmpfs;

/// Every mount option `Tmpfs$mountOptions` accepts, as that member publishes them, in the
/// order the doc lists them.
pub const TMPFS_MOUNT_OPTIONS: [&str; 39] = [
    "defaults",
    "ro",
    "rw",
    "suid",
    "nosuid",
    "dev",
    "nodev",
    "exec",
    "noexec",
    "sync",
    "async",
    "dirsync",
    "remount",
    "mand",
    "nomand",
    "atime",
    "noatime",
    "diratime",
    "nodiratime",
    "bind",
    "rbind",
    "unbindable",
    "runbindable",
    "private",
    "rprivate",
    "shared",
    "rshared",
    "slave",
    "rslave",
    "relatime",
    "norelatime",
    "strictatime",
    "nostrictatime",
    "mode",
    "uid",
    "gid",
    "nr_inodes",
    "nr_blocks",
    "mpol",
];

/// `ClientException$message` for a tmpfs mount option that names no mount option.
///
/// Amazon ECS states the ONE option at fault, as the request spelled it, and nothing else of
/// the request: that option is the only value the caller has to change. RECORDED behaviour.
pub fn invalid_tmpfs_mount_option_message(option: &str) -> String {
    format!("The {option} tmpfs mount option specified is invalid.")
}

/// The first option of `mounts` that is outside the catalog, stated as the refusal's message —
/// or `None` when every option of every mount names one (and for a container that mounts
/// none).
///
/// The mounts are walked in the order the request lists them, and each one's options in the
/// order it declares them, so the refusal a container earns never depends on anything but its
/// values.
pub fn tmpfs_mount_option_violation(mounts: &[Tmpfs]) -> Option<String> {
    mounts
        .iter()
        .flat_map(|mount| mount.mount_options.iter().flatten())
        .find(|option| !TMPFS_MOUNT_OPTIONS.contains(&option.as_str()))
        .map(|option| invalid_tmpfs_mount_option_message(option))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One tmpfs mount declaring `options`.
    fn tmpfs(options: &[&str]) -> Tmpfs {
        Tmpfs {
            container_path: "/run".to_string(),
            size: 64,
            mount_options: Some(options.iter().map(|option| (*option).to_string()).collect()),
        }
    }

    /// RULE: an option the catalog holds is accepted; one it does not is refused, stating the
    /// option as the request spelled it.
    #[test]
    fn an_option_outside_the_catalog_is_refused() {
        // (case, the options one mount declares, the sentence it earns)
        let rows: &[(&str, &[&str], Option<&str>)] = &[
            ("no options", &[], None),
            ("catalog options", &["noexec", "nosuid", "rw"], None),
            // An entry of the catalog the issue this rule was found on did not cite.
            ("another catalog option", &["nr_inodes"], None),
            (
                "an upper-case option",
                &["NOEXEC"],
                Some("The NOEXEC tmpfs mount option specified is invalid."),
            ),
            (
                "an option nothing publishes",
                &["nowhere"],
                Some("The nowhere tmpfs mount option specified is invalid."),
            ),
            (
                "a catalog option beside one outside it",
                &["rw", "NOSUID"],
                Some("The NOSUID tmpfs mount option specified is invalid."),
            ),
        ];
        for (case, options, message) in rows {
            let mounts = [tmpfs(options)];
            let answer = tmpfs_mount_option_violation(&mounts);
            assert_eq!(
                answer.as_deref(),
                *message,
                "case {case:?}: expected {message:?}, got {answer:?}"
            );
        }
    }

    /// RULE: the rule is read over every mount the container declares, not only the first.
    #[test]
    fn every_mount_of_the_container_is_weighed() {
        let mounts = [tmpfs(&["rw"]), tmpfs(&["NODEV"])];
        let answer = tmpfs_mount_option_violation(&mounts);
        assert_eq!(
            answer.as_deref(),
            Some("The NODEV tmpfs mount option specified is invalid."),
            "case \"a later mount at fault\": expected the option of the second mount, got \
             {answer:?}"
        );
        let none = tmpfs_mount_option_violation(&[]);
        assert!(
            none.is_none(),
            "case \"no mount at all\": expected the container to be accepted, got {none:?}"
        );
    }
}

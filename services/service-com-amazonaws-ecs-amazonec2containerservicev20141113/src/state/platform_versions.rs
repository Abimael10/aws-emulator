// SPDX-License-Identifier: CC0-1.0

//! Which Fargate PLATFORM VERSION a task of a given revision could be placed on.
//!
//! The platform versions are published per operating system, so a version is a version OF
//! one of the `OSFamily` values — which makes the pairing a cross-field rule, keyed on the
//! `runtimePlatform` the registration stored rather than on anything the request carries.
//! Read by every operation that pairs a `platformVersion` with a revision: `RunTask`,
//! `CreateService`, `UpdateService` and `CreateTaskSet`.

use super::*;

/// The Fargate platform versions that WERE published and have since been RETIRED, as the
/// `platformVersion` doc's "Fargate platform versions" page keeps them under its
/// deprecation notice: the Linux versions before the current one
/// ([`LATEST_LINUX_PLATFORM_VERSION`]).
///
/// A retired version is not a value that never named a platform — it named one Fargate no
/// longer runs, and no account may place a task on it any more, which is the entitlement
/// `AccessDeniedException` ("You don't have authorization to perform the requested action")
/// is the class for. `1.0.0` is NOT one of them: that string is the Windows platform
/// version Fargate still publishes ([`LATEST_WINDOWS_PLATFORM_VERSION`]).
pub const RETIRED_PLATFORM_VERSIONS: [&str; 3] = ["1.1.0", "1.2.0", "1.3.0"];

/// Whether a request's `platformVersion` names a platform version Fargate has RETIRED
/// ([`RETIRED_PLATFORM_VERSIONS`]).
///
/// Asked BEFORE [`is_unknown_platform_version`], which answers for the other values outside
/// the published list: the two conditions are different — a version that once existed and is
/// gone is refused as the entitlement it is, a value that never named one as the unknown
/// platform it is — so the operations that pair a `platformVersion` with a placement answer
/// them in two different modelled error classes.
pub fn names_retired_platform_version(platform_version: Option<&str>) -> bool {
    requested_platform_version(platform_version)
        .is_some_and(|version| RETIRED_PLATFORM_VERSIONS.contains(&version))
}

/// `AccessDeniedException$message` for a placement that names a retired Fargate platform
/// version: the whole sentence is FIXED text, as the recorded refusal carries it — it states
/// the entitlement the account does not have on the platform it asked for, and names neither
/// the version the request carried nor a version it could use instead.
pub const PLATFORM_ACCESS_DENIED_MESSAGE: &str =
    "You do not have authorization to access the specified platform.";

/// Whether a requested `platformVersion` names no platform the revision's tasks could be
/// placed on, because it is not published for the operating system they run.
///
/// The Fargate platform versions are published PER OPERATING SYSTEM, and the WINDOWS
/// families have exactly one of them ([`LATEST_WINDOWS_PLATFORM_VERSION`]): the Linux
/// versions are not published for Windows containers at all, so a Windows revision paired
/// with one names a platform that does not exist — which is what
/// `PlatformUnknownException` ("The specified platform version doesn't exist.") is
/// modelled for, exactly as a value that was never a version is.
/// [`PLATFORM_VERSION_LATEST`] names no version of either, so it resolves against
/// whichever family the revision declares and always exists.
///
/// The Linux families are NOT held to one version in the same way: Linux has had several
/// published platform versions, and which of them a task may still be placed on is what
/// [`is_unknown_platform_version`] decides, before this check.
///
/// Three string comparisons over values this function only reads: it parses nothing,
/// indexes nothing and allocates nothing, whatever the caller spelled.
pub fn names_no_platform_for_os_family(requested: Option<&str>, os_family: &str) -> bool {
    let Some(requested) = requested else {
        return false;
    };
    requested != PLATFORM_VERSION_LATEST
        && is_windows_os_family(os_family)
        && requested != LATEST_WINDOWS_PLATFORM_VERSION
}

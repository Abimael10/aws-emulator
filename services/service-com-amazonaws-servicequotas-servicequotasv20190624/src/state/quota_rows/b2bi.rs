// SPDX-License-Identifier: CC0-1.0

//! B2B Data Interchange's published default-quota catalog.

use super::*;

/// B2B Data Interchange's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `b2bi` service code, which states
/// which quota each `QuotaCode` names and the default published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order — case 1
/// of the rule [`crate::state::descriptive_quota_code`] states. The profile row this
/// catalog used to publish under the code the service's OWN
/// `ServiceQuotaExceededException` reports (`ProfilesPerAccount`, case 2 of that rule) is
/// reported by that listing as a surplus entry: AWS publishes that quota under
/// `L-05F6A9EF`, so the account's profile limit is published under that code here and
/// the service-cited stand-in is not published at all.
///
/// The profile limit is also documented: `CreateProfile` in the b2bi model states "You
/// can have up to five customer profiles, each representing a distinct private network",
/// and the b2bi emulator enforces exactly this number. No cross-crate dependency links
/// the two, so keep it in step with `MAX_PROFILES_PER_ACCOUNT` in
/// `services/service-com-amazonaws-b2bi-b2bi/src/state.rs` (each side has a test pinning
/// it).
pub(in crate::state) fn b2bi_quotas() -> Vec<QuotaRow> {
    vec![
        // The account's own resource planes, one row per resource the service creates.
        quota("Profiles", 5.0).code("L-05F6A9EF"),
        quota("Capabilities", 100.0).code("L-19B5E505"),
        quota("Partnerships", 700.0).code("L-CD5D2786"),
        quota("Transformers", 500.0).code("L-E15C983E"),
    ]
}

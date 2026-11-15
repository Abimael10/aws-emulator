// SPDX-License-Identifier: CC0-1.0

// Empty, and declared by nothing: the `date-time` timestamp codec this service takes is the
// workspace's shared `service_common::date_time_json::DateTimeRestJson` — held in
// `super::EmulatedService::protocol` and taken at `Service::override_protocol` — rather than
// a copy of that rule under this crate. Nothing belongs in this file; a service-local
// difference from the shared codec would be a new module named for that difference.

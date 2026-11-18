// SPDX-License-Identifier: CC0-1.0

//! Not part of the crate: no module declares this file and it holds no code.
//!
//! What a VPC's own DNS answers is read, for every client of a VPC in this emulator, by the DNS
//! data plane of the Route 53 Resolver emulator (`state::vpc_dns` there) and by the Lambda
//! runtime's own `state::execution::vpc_network`; this file held a third reading of it, which
//! was dropped so no third exists. The tooling available here cannot delete a file once
//! written, so what is left of it is this note.

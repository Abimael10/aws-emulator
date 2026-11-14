// SPDX-License-Identifier: CC0-1.0

//! EMPTY. The `posix` module a code interpreter run is built on is NOT this crate's: the one
//! contained interpreter this workspace embeds supplies it
//! (`service_com_amazonaws_lambda_awsgirapiservice::state::execution::host_posix`), with
//! every host-reaching call refused, and [`super::code_interpreter_python`] drives that
//! engine rather than a second one. This file held a second copy of that module and is no
//! longer declared anywhere. It stays only because this fixer's tool set can write a file but
//! not remove one; the next change to touch this directory should delete it.

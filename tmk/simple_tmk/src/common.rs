// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Simple tests common to all architectures.

use crate::prelude::*;

#[tmk_test]
fn boot(_: TestContext<'_>) {
    log!("hello world");
}

// core::arch::global_asm! {
//     "instruction_abort_outside_par_trampoline:",
//     "movz x16, #0x0000",
//     "movk x16, #0x0000, lsl #16",
//     "movk x16, #0xffff, lsl #32",
//     "movk x16, #0x0000, lsl #48",
//     "br x16",
// }

// #[tmk_test]
// fn instruction_abort_outside_par(_: TestContext<'_>) {
//     log!("instruction_abort_outside_par");

//     instruction_abort_outside_par_trampoline();

//     panic!("branch to outside PAR unexpectedly returned");
// }
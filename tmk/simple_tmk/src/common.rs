// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Simple tests common to all architectures.

use crate::prelude::*;

#[tmk_test]
fn boot(_: TestContext<'_>) {
    log!("hello world");
}

#[tmk_test]
fn instruction_abort_outside_par(_: TestContext<'_>) {
    log!("instruction_abort_outside_par");

    let target = 0x0000_ffff_0000_0000usize;

    unsafe {
        let f: extern "C" fn() = core::mem::transmute(target);
        f();
    }

    panic!("branch to outside PAR unexpectedly returned");
}
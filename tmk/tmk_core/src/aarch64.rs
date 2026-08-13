// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Aarch64 entry point and support.

#![cfg(target_arch = "aarch64")]

use super::Scope;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::Relaxed;

const GIC_DISTRIBUTOR_BASE: usize = tmk_protocol::aarch64::GIC_DISTRIBUTOR_BASE as usize;
const GIC_REDISTRIBUTOR_BASE: usize = tmk_protocol::aarch64::GIC_REDISTRIBUTOR_BASE as usize;

const GICR_SGI_FRAME_OFFSET: usize = 0x1_0000;
const GICD_CTLR_OFFSET: usize = 0x0;
const GICR_WAKER_OFFSET: usize = 0x14;
const GICR_IGROUPR0_OFFSET: usize = 0x80;
const GICR_ISENABLER0_OFFSET: usize = 0x100;
const GICR_ICENABLER0_OFFSET: usize = 0x180;
const GICR_IPRIORITYR0_OFFSET: usize = 0x400;

const GIC_REDISTRIBUTOR_SGI_BASE: usize = GIC_REDISTRIBUTOR_BASE + GICR_SGI_FRAME_OFFSET;
const GICD_CTLR_ADDRESS: usize = GIC_DISTRIBUTOR_BASE + GICD_CTLR_OFFSET;
const GICR_WAKER_ADDRESS: usize = GIC_REDISTRIBUTOR_BASE + GICR_WAKER_OFFSET;
const GICR_IGROUPR0_ADDRESS: usize = GIC_REDISTRIBUTOR_SGI_BASE + GICR_IGROUPR0_OFFSET;
const GICR_ISENABLER0_ADDRESS: usize = GIC_REDISTRIBUTOR_SGI_BASE + GICR_ISENABLER0_OFFSET;
const GICR_ICENABLER0_ADDRESS: usize = GIC_REDISTRIBUTOR_SGI_BASE + GICR_ICENABLER0_OFFSET;
const GICR_IPRIORITYR0_ADDRESS: usize = GIC_REDISTRIBUTOR_SGI_BASE + GICR_IPRIORITYR0_OFFSET;

const GIC_SPECIAL_INTID: u32 = 1020;
const DAIF_IRQ_MASK: u64 = 1 << 7;
const GICD_CTLR_ENABLE_GRP1_AND_ARE: u32 = (1 << 1) | (1 << 4);

/// GIC interrupt ID used by the architectural virtual timer.
pub const VIRTUAL_TIMER_PPI: u32 = tmk_protocol::aarch64::VIRTUAL_TIMER_PPI;

static ARCH_INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut IRQ_HANDLER: [usize; 2] = [0; 2];

/// A context passed to the IRQ handler.
pub struct IrqContext {
    /// The GIC interrupt ID being handled.
    pub intid: u32,
}

#[cfg(minimal_rt)]
mod entry {
    core::arch::global_asm! {
        ".extern _DYNAMIC",
        ".globl _start",
        "_start:",
        "mov x19, x0",
        "adrp x1, {stack}",
        "add x1, x1, :lo12:{stack}",
        "add x1, x1, {STACK_SIZE}",
        "mov sp, x1",

        // Enable the FPU.
        "mrs     x0, CPACR_EL1",
        "orr     x0, x0, #(3 << 20)",
        "orr     x0, x0, #(3 << 16)",
        "msr     CPACR_EL1, x0",
        "isb",

        "adrp x0, __ehdr_start",
        "add x0, x0, :lo12:__ehdr_start",
        "mov x1, x0",
        "adrp x2, _DYNAMIC",
        "add x2, x2, :lo12:_DYNAMIC",
        "bl {relocate}",
        "mov x0, x19",
        "b {entry}",
        relocate = sym minimal_rt::reloc::relocate,
        stack = sym STACK,
        entry = sym crate::entry,
        STACK_SIZE = const STACK_SIZE,
    }

    const STACK_SIZE: usize = 16384;
    #[repr(C, align(16))]
    struct Stack([u8; STACK_SIZE]);
    static mut STACK: Stack = Stack([0; STACK_SIZE]);
}

core::arch::global_asm! {
    ".balign 2048",
    ".globl {exception_vectors}",
    "{exception_vectors}:",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x080",
    "b irq_exception",
    ".org {exception_vectors} + 0x100",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x180",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x200",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x280",
    "b irq_exception",
    ".org {exception_vectors} + 0x300",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x380",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x400",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x480",
    "b irq_exception",
    ".org {exception_vectors} + 0x500",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x580",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x600",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x680",
    "b irq_exception",
    ".org {exception_vectors} + 0x700",
    "b unexpected_exception",
    ".org {exception_vectors} + 0x780",
    "b unexpected_exception",

    "irq_exception:",
    "sub sp, sp, #256",
    "stp x0, x1, [sp, #0]",
    "stp x2, x3, [sp, #16]",
    "stp x4, x5, [sp, #32]",
    "stp x6, x7, [sp, #48]",
    "stp x8, x9, [sp, #64]",
    "stp x10, x11, [sp, #80]",
    "stp x12, x13, [sp, #96]",
    "stp x14, x15, [sp, #112]",
    "stp x16, x17, [sp, #128]",
    "stp x18, x19, [sp, #144]",
    "stp x20, x21, [sp, #160]",
    "stp x22, x23, [sp, #176]",
    "stp x24, x25, [sp, #192]",
    "stp x26, x27, [sp, #208]",
    "stp x28, x29, [sp, #224]",
    "str x30, [sp, #240]",
    "bl {irq_handler}",
    "ldr x30, [sp, #240]",
    "ldp x28, x29, [sp, #224]",
    "ldp x26, x27, [sp, #208]",
    "ldp x24, x25, [sp, #192]",
    "ldp x22, x23, [sp, #176]",
    "ldp x20, x21, [sp, #160]",
    "ldp x18, x19, [sp, #144]",
    "ldp x16, x17, [sp, #128]",
    "ldp x14, x15, [sp, #112]",
    "ldp x12, x13, [sp, #96]",
    "ldp x10, x11, [sp, #80]",
    "ldp x8, x9, [sp, #64]",
    "ldp x6, x7, [sp, #48]",
    "ldp x4, x5, [sp, #32]",
    "ldp x2, x3, [sp, #16]",
    "ldp x0, x1, [sp, #0]",
    "add sp, sp, #256",
    "eret",

    "unexpected_exception:",
    "b {unexpected_exception_handler}",
    exception_vectors = sym EXCEPTION_VECTORS,
    irq_handler = sym irq_handler,
    unexpected_exception_handler = sym unexpected_exception_handler,
}

unsafe extern "C" {
    safe static EXCEPTION_VECTORS: u8;
}

pub(super) struct ArchScopeState {
    old_irq_handler: Option<[usize; 2]>,
    interrupts_enabled: bool,
}

impl<'scope> Scope<'scope, '_> {
    pub(super) fn arch_init() -> ArchScopeState {
        arch_init_once();
        ArchScopeState {
            old_irq_handler: None,
            interrupts_enabled: are_interrupts_enabled(),
        }
    }

    pub(super) fn arch_reset(&mut self) {
        if let Some(handler) = self.arch.old_irq_handler.take() {
            let _disable = disable_guarded();
            // SAFETY: IRQ_HANDLER is not concurrently accessed while IRQs are disabled.
            unsafe { IRQ_HANDLER = handler };
        }
        if self.arch.interrupts_enabled {
            enable_interrupts();
        } else {
            disable_interrupts();
        }
    }

    fn save_irq_handler(&mut self) {
        if self.arch.old_irq_handler.is_some() {
            return;
        }
        let _disable = disable_guarded();
        // SAFETY: IRQ_HANDLER is not concurrently modified while IRQs are disabled.
        self.arch.old_irq_handler = Some(unsafe { IRQ_HANDLER });
    }

    /// Sets the IRQ handler.
    ///
    /// This is reverted when the scope ends.
    pub fn set_irq_handler(&mut self, handler: &'scope (dyn Send + Fn(&mut IrqContext))) {
        self.save_irq_handler();
        let _disable = disable_guarded();
        // SAFETY: IRQ_HANDLER is not concurrently modified while IRQs are disabled.
        unsafe {
            IRQ_HANDLER =
                core::mem::transmute::<&(dyn Send + Fn(&mut IrqContext)), [usize; 2]>(handler)
        };
    }

    /// Enables a GIC SGI/PPI interrupt for the current VP.
    pub fn enable_gic_irq(&self, intid: u32) {
        assert!(intid < 32, "only SGI/PPI interrupts are supported");
        enable_gic_for_current_vp();
        write_reg32(
            GICR_IGROUPR0_ADDRESS,
            read_reg32(GICR_IGROUPR0_ADDRESS) | (1 << intid),
        );

        let priority_address = GICR_IPRIORITYR0_ADDRESS + (intid as usize & !3);
        let priority_shift = (intid & 3) * 8;
        let priority = read_reg32(priority_address) & !(0xff << priority_shift);
        write_reg32(priority_address, priority | (0x80 << priority_shift));
        write_reg32(GICR_ISENABLER0_ADDRESS, 1 << intid);
        memory_barrier();
    }

    /// Disables a GIC SGI/PPI interrupt for the current VP.
    pub fn disable_gic_irq(&self, intid: u32) {
        assert!(intid < 32, "only SGI/PPI interrupts are supported");
        write_reg32(GICR_ICENABLER0_ADDRESS, 1 << intid);
        memory_barrier();
    }

    /// Enables IRQ delivery.
    ///
    /// This is reverted when the scope ends.
    pub fn enable_interrupts(&self) {
        enable_interrupts();
    }

    /// Disables IRQ delivery and returns true if it was previously enabled.
    ///
    /// This is reverted when the scope ends.
    pub fn disable_interrupts(&self) -> bool {
        disable_interrupts()
    }
}

/// Reads the virtual counter frequency.
pub fn read_cntfrq() -> u64 {
    let value;
    // SAFETY: reading CNTFRQ_EL0 is side-effect free.
    unsafe {
        core::arch::asm!("mrs {value}, CNTFRQ_EL0", value = out(reg) value);
    }
    value
}

/// Reads the virtual counter.
pub fn read_cntvct() -> u64 {
    let value;
    // SAFETY: reading CNTVCT_EL0 is side-effect free.
    unsafe {
        core::arch::asm!("isb; mrs {value}, CNTVCT_EL0", value = out(reg) value);
    }
    value
}

/// Arms the virtual timer to fire at `count`.
pub fn set_virtual_timer_compare(count: u64) {
    // SAFETY: programming the virtual timer is expected in TMK tests.
    unsafe {
        core::arch::asm!(
            "msr CNTV_CVAL_EL0, {count}",
            "msr CNTV_CTL_EL0, {control}",
            "isb",
            count = in(reg) count,
            control = in(reg) 1u64,
        );
    }
}

/// Disables the virtual timer.
pub fn disable_virtual_timer() {
    // SAFETY: programming the virtual timer is expected in TMK tests.
    unsafe {
        core::arch::asm!(
            "msr CNTV_CTL_EL0, {control}",
            "isb",
            control = in(reg) 0u64,
        );
    }
}

/// Polls the emulated GIC while waiting for an interrupt.
///
/// This gives VMMs that emulate the GIC a regular exit on which to observe
/// changes to interrupt sources that are managed outside the guest.
pub fn poll_interrupts() {
    let _ = read_reg32(GICD_CTLR_ADDRESS);
}

#[cfg_attr(not(minimal_rt), expect(dead_code))]
extern "C" fn irq_handler() {
    let intid = read_icc_iar1();
    if intid >= GIC_SPECIAL_INTID {
        return;
    }

    let handler = {
        // SAFETY: IRQ_HANDLER is changed only while IRQs are disabled.
        let handler = unsafe { IRQ_HANDLER };
        // SAFETY: this is the underlying type stored by set_irq_handler.
        unsafe {
            core::mem::transmute::<[usize; 2], Option<&(dyn Send + Fn(&mut IrqContext))>>(handler)
        }
    };

    let Some(handler) = handler else {
        panic!("unhandled IRQ {intid}");
    };

    handler(&mut IrqContext { intid });
    write_icc_eoir1(intid);
}

#[cfg_attr(not(minimal_rt), expect(dead_code))]
extern "C" fn unexpected_exception_handler() -> ! {
    let esr: u64;
    let elr: u64;
    let far: u64;
    // SAFETY: reading exception syndrome registers is side-effect free.
    unsafe {
        core::arch::asm!(
            "mrs {esr}, ESR_EL1",
            "mrs {elr}, ELR_EL1",
            "mrs {far}, FAR_EL1",
            esr = out(reg) esr,
            elr = out(reg) elr,
            far = out(reg) far,
        );
    }
    panic!("unexpected exception: esr={esr:#x}, elr={elr:#x}, far={far:#x}");
}

fn arch_init_once() {
    if ARCH_INITIALIZED.swap(true, Relaxed) {
        return;
    }

    disable_interrupts();
    // SAFETY: installing the EL1 exception vector is expected during TMK init.
    unsafe {
        core::arch::asm!(
            "msr VBAR_EL1, {vectors}",
            "isb",
            vectors = in(reg) &EXCEPTION_VECTORS as *const u8 as u64,
        );
    }
}

fn enable_gic_for_current_vp() {
    write_reg32(GICD_CTLR_ADDRESS, GICD_CTLR_ENABLE_GRP1_AND_ARE);

    let waker = read_reg32(GICR_WAKER_ADDRESS) & !(1 << 1);
    write_reg32(GICR_WAKER_ADDRESS, waker);
    while read_reg32(GICR_WAKER_ADDRESS) & (1 << 2) != 0 {
        core::hint::spin_loop();
    }

    // SAFETY: programming the GIC CPU system-register interface is expected in TMK tests.
    unsafe {
        core::arch::asm!(
            "msr ICC_SRE_EL1, {sre}",
            "isb",
            "msr ICC_PMR_EL1, {pmr}",
            "msr ICC_IGRPEN1_EL1, {enable}",
            "isb",
            sre = in(reg) 1u64,
            pmr = in(reg) 0xffu64,
            enable = in(reg) 1u64,
        );
    }
}

fn read_icc_iar1() -> u32 {
    let value: u64;
    // SAFETY: reading ICC_IAR1_EL1 acknowledges the pending interrupt.
    unsafe {
        core::arch::asm!("mrs {value}, ICC_IAR1_EL1", value = out(reg) value);
    }
    value as u32
}

fn write_icc_eoir1(intid: u32) {
    // SAFETY: writing ICC_EOIR1_EL1 completes handling of `intid`.
    unsafe {
        core::arch::asm!("msr ICC_EOIR1_EL1, {value}", value = in(reg) intid as u64);
    }
}

fn read_reg32(address: usize) -> u32 {
    // SAFETY: callers pass known GIC MMIO register addresses.
    unsafe { (address as *const u32).read_volatile() }
}

fn write_reg32(address: usize, value: u32) {
    // SAFETY: callers pass known GIC MMIO register addresses.
    unsafe { (address as *mut u32).write_volatile(value) }
}

fn memory_barrier() {
    // SAFETY: a barrier has no memory-safety requirements.
    unsafe {
        core::arch::asm!("dsb sy", "isb");
    }
}

#[must_use]
struct DisableGuard(bool);

fn disable_guarded() -> DisableGuard {
    DisableGuard(disable_interrupts())
}

impl Drop for DisableGuard {
    fn drop(&mut self) {
        if self.0 {
            enable_interrupts();
        }
    }
}

fn disable_interrupts() -> bool {
    let enabled = are_interrupts_enabled();
    if enabled {
        // SAFETY: disabling IRQs is always memory safe.
        unsafe {
            core::arch::asm!("msr daifset, #2");
        }
    }
    enabled
}

fn enable_interrupts() {
    // SAFETY: callers ensure an IRQ handler is installed first.
    unsafe {
        core::arch::asm!("msr daifclr, #2", "isb");
    }
}

fn are_interrupts_enabled() -> bool {
    let daif: u64;
    // SAFETY: reading DAIF is side-effect free.
    unsafe {
        core::arch::asm!("mrs {daif}, DAIF", daif = out(reg) daif);
    }
    daif & DAIF_IRQ_MASK == 0
}

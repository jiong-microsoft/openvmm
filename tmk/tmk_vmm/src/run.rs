// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Support for running a VM's VPs.

use crate::Options;
use crate::load;
use anyhow::Context as _;
use futures::StreamExt as _;
use guestmem::GuestMemory;
use hvdef::Vtl;
#[cfg(guest_arch = "aarch64")]
use memory_range::MemoryRange;
use pal_async::DefaultDriver;
use std::sync::Arc;
#[cfg(guest_arch = "aarch64")]
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use user_driver::DmaClient;
use virt::PartitionCapabilities;
use virt::Processor;
use virt::StopVpSource;
use virt::VpIndex;
use virt::io::CpuIo;
use virt::vp::AccessVpState as _;
use vm_topology::memory::MemoryLayout;
use vm_topology::processor::ProcessorTopology;
use vm_topology::processor::TopologyBuilder;
#[cfg(guest_arch = "aarch64")]
use vm_topology::processor::aarch64::GicVersion;
use vmcore::vmtime::VmTime;
use vmcore::vmtime::VmTimeKeeper;
use vmcore::vmtime::VmTimeSource;
use zerocopy::TryFromBytes as _;

#[cfg(guest_arch = "aarch64")]
use std::collections::VecDeque;
#[cfg(guest_arch = "aarch64")]
use std::io::Read as _;
#[cfg(guest_arch = "aarch64")]
use std::io::Write as _;

pub const COMMAND_ADDRESS: u64 = 0xffff_0000;

#[cfg(guest_arch = "aarch64")]
const UART_BASE: u64 = 0xeffe_c000;
#[cfg(guest_arch = "aarch64")]
const UART_SIZE: u64 = 0x1000;
#[cfg(guest_arch = "aarch64")]
const UART_INTID: u32 = 33;
#[cfg(guest_arch = "aarch64")]
const PL011_INT_RX: u32 = 1 << 4;
#[cfg(guest_arch = "aarch64")]
const PL011_INT_RT: u32 = 1 << 6;

#[cfg(guest_arch = "aarch64")]
struct PlatformGic {
    distributor: virt_support_gic::Distributor,
    distributor_range: MemoryRange,
    redistributor_range: MemoryRange,
}

#[cfg(guest_arch = "aarch64")]
impl PlatformGic {
    fn new(topology: &ProcessorTopology) -> anyhow::Result<Self> {
        let redistributors_base = match topology.gic_version() {
            GicVersion::V3 {
                redistributors_base,
            } => redistributors_base,
            GicVersion::V2 { .. } => anyhow::bail!("CCA Linux prototype requires GICv3"),
        };
        let redistributors_size = aarch64defs::GIC_REDISTRIBUTOR_SIZE
            .checked_mul(u64::from(topology.vp_count()))
            .context("GIC redistributor range overflowed")?;
        let redistributors_end = redistributors_base
            .checked_add(redistributors_size)
            .context("GIC redistributor range overflowed")?;
        let redistributor_range = MemoryRange::new(redistributors_base..redistributors_end);
        let distributor_base = topology.gic_distributor_base();
        let distributor_end = distributor_base
            .checked_add(aarch64defs::GIC_DISTRIBUTOR_SIZE)
            .context("GIC distributor range overflowed")?;
        let distributor_range = MemoryRange::new(distributor_base..distributor_end);

        let mut distributor = virt_support_gic::Distributor::new(
            distributor_base,
            redistributor_range,
            topology.gic_nr_irqs(),
        );
        let vp_count = topology.vp_count() as usize;
        for (index, vp) in topology.vps_arch().enumerate() {
            distributor.add_redistributor(vp.mpidr.into(), index + 1 == vp_count);
        }

        Ok(Self {
            distributor,
            distributor_range,
            redistributor_range,
        })
    }

    fn contains(&self, address: u64) -> bool {
        self.distributor_range.contains_addr(address)
            || self.redistributor_range.contains_addr(address)
    }

    fn read(&self, address: u64, data: &mut [u8]) -> bool {
        self.distributor.read(address, data)
    }

    fn write(&self, address: u64, data: &[u8]) -> bool {
        self.distributor.write(address, data)
    }
}

#[cfg(guest_arch = "aarch64")]
struct SerialState {
    line: String,
    input: VecDeque<u8>,
    interrupt_mask: u32,
    interrupt_asserted: bool,
    success_marker: Option<String>,
    completed: bool,
    raw_output: bool,
    raw_interactive: bool,
}

#[cfg(guest_arch = "aarch64")]
struct SerialDevice {
    state: Mutex<SerialState>,
    interrupt: Option<std::sync::Weak<dyn virt::irqcon::ControlGic>>,
}

#[cfg(guest_arch = "aarch64")]
impl SerialDevice {
    fn new(
        success_marker: Option<String>,
        raw_output: bool,
        interrupt: Option<&Arc<dyn virt::irqcon::ControlGic>>,
    ) -> Self {
        Self {
            state: Mutex::new(SerialState {
                line: String::new(),
                input: VecDeque::new(),
                interrupt_mask: 0,
                interrupt_asserted: false,
                success_marker,
                completed: false,
                raw_output,
                raw_interactive: false,
            }),
            interrupt: interrupt.map(Arc::downgrade),
        }
    }

    fn start_input_reader(self: &Arc<Self>) {
        let serial = Arc::clone(self);
        std::thread::spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut buffer = [0; 256];

            loop {
                match input.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(len) => serial.queue_input(&buffer[..len]),
                }
            }
        });
    }

    fn queue_input(&self, input: &[u8]) {
        let update = {
            let mut state = self.state.lock().expect("serial mutex poisoned");
            state.raw_interactive = state.raw_output;
            state.input.extend(input.iter().copied());
            Self::update_interrupt(&mut state)
        };
        self.apply_interrupt_update(update);
    }

    fn update_interrupt(state: &mut SerialState) -> Option<bool> {
        let asserted =
            !state.input.is_empty() && state.interrupt_mask & (PL011_INT_RX | PL011_INT_RT) != 0;
        if asserted == state.interrupt_asserted {
            return None;
        }

        state.interrupt_asserted = asserted;
        Some(asserted)
    }

    fn apply_interrupt_update(&self, update: Option<bool>) {
        let Some(asserted) = update else {
            return;
        };
        let Some(interrupt) = self.interrupt.as_ref().and_then(|gic| gic.upgrade()) else {
            return;
        };
        interrupt.set_spi_irq(UART_INTID, asserted);
    }
}

#[cfg(all(target_os = "linux", guest_arch = "aarch64"))]
mod cca {
    use super::DmaClient;
    use super::MemoryLayout;
    use super::Options;
    use crate::HypervisorOpt;
    use anyhow::Context as _;
    use core::ops::Range;
    use memory_range::MemoryRange;
    use std::sync::Arc;
    use underhill_mem::MemoryAcceptor;
    use user_driver::lockmem::LockedMemorySpawner;
    use user_driver::memory::MemoryBlock;
    use user_driver::memory::PAGE_SIZE;
    use user_driver::memory::PAGE_SIZE64;
    use virt::IsolationType;
    use vm_topology::memory::MemoryRangeWithNode;

    pub(super) struct CcaState {
        pub(super) private_dma_client: Arc<dyn DmaClient>,
        pub(super) _guest_ram_backing: MemoryBlock,
    }

    pub(super) fn build(
        opts: &Options,
        memory_layout: &mut MemoryLayout,
        ram_size: u64,
    ) -> anyhow::Result<Option<CcaState>> {
        let hv = opts.hv.expect("hv must have a finalized value");
        match hv {
            HypervisorOpt::Cca => {
                let private_dma_client: Arc<dyn DmaClient> = Arc::new(LockedMemorySpawner);

                let (private_memory, private_ram_pfn) = {
                    const BITMAP_ALIGNMENT: u64 = PAGE_SIZE64 * 8;
                    const MAX_ALLOC_ATTEMPTS: usize = 4;
                    let asking_size = ram_size
                        .checked_add(BITMAP_ALIGNMENT - PAGE_SIZE64)
                        .context("private CCA RAM search size overflowed")?;
                    let mut map_size = usize::try_from(asking_size)
                        .context("private CCA RAM search size does not fit usize")?;
                    let mut selected = None;

                    for _attempt in 0..MAX_ALLOC_ATTEMPTS {
                        let private_memory = private_dma_client
                            .allocate_dma_buffer(map_size)
                            .with_context(|| {
                                format!(
                                    "failed to allocate private CCA RAM buffer of size {map_size}"
                                )
                            })?;

                        if let Some(pfns) =
                            contiguous_subpfns(&private_memory, asking_size as usize)
                        {
                            let page_count = (ram_size as usize).div_ceil(PAGE_SIZE);
                            if let Some(start_index) = pfns
                                .iter()
                                .position(|pfn| {
                                    (pfn * PAGE_SIZE64).is_multiple_of(BITMAP_ALIGNMENT)
                                })
                                .filter(|&start_index| pfns.len() - start_index >= page_count)
                            {
                                selected = Some((private_memory, pfns[start_index]));
                                break;
                            }
                        }

                        map_size = map_size
                            .checked_mul(2)
                            .context("private CCA RAM allocation size overflowed while retrying")?;
                    }

                    selected.with_context(|| {
                        format!(
                            "failed to allocate private CCA RAM with {ram_size} contiguous bytes after {MAX_ALLOC_ATTEMPTS} attempts"
                        )
                    })?
                };

                private_memory.write_zeros(0, private_memory.len());

                let pa = private_ram_pfn * PAGE_SIZE64;
                let start = pa;
                let end = pa
                    .checked_add(ram_size)
                    .context("private CCA RAM range overflowed")?;

                *memory_layout = MemoryLayout::new_from_ranges(
                    &[MemoryRangeWithNode {
                        range: MemoryRange::new(Range { start, end }),
                        vnode: 0,
                    }],
                    &[],
                )
                .context("bad memory layout")?;

                // Grant GPA to Plane1 (eqv. VTL0)
                let ram = memory_layout.ram().iter().map(|r| r.range);
                let acceptor = MemoryAcceptor::new(IsolationType::Cca)?;
                for range in ram {
                    acceptor.apply_initial_lower_vtl_protections(range)?;
                }

                Ok(Some(CcaState {
                    private_dma_client,
                    _guest_ram_backing: private_memory,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Returns a sorted contiguous subset of PFNs large enough for `asking_size` bytes.
    fn contiguous_subpfns(memory: &MemoryBlock, asking_size: usize) -> Option<Vec<u64>> {
        let page_count = asking_size.div_ceil(PAGE_SIZE);
        if page_count == 0 {
            return Some(Vec::new());
        }

        let mut pfns = memory.pfns().to_vec();
        pfns.sort_unstable();

        let mut run_start = 0;
        for i in 1..=pfns.len() {
            let run_ended = i == pfns.len() || pfns[i - 1] + 1 != pfns[i];
            if run_ended {
                if i - run_start >= page_count {
                    pfns.truncate(run_start + page_count);
                    pfns.drain(..run_start);
                    return Some(pfns);
                }
                run_start = i;
            }
        }

        None
    }
}

pub struct CommonState {
    pub driver: DefaultDriver,
    pub opts: Options,
    pub processor_topology: ProcessorTopology,
    pub memory_layout: MemoryLayout,
    #[cfg(all(target_os = "linux", guest_arch = "aarch64"))]
    cca: Option<cca::CcaState>,
}

pub struct RunContext<'a> {
    pub state: &'a CommonState,
    pub vmtime_source: &'a VmTimeSource,
}

#[derive(Debug, Clone)]
pub enum TestResult {
    Passed,
    Failed,
    Faulted {
        vp_index: VpIndex,
        reason: String,
        regs: Option<Box<virt::vp::Registers>>,
    },
}

impl CommonState {
    #[cfg(all(target_os = "linux", guest_arch = "aarch64"))]
    pub fn cca_private_dma_client(&self) -> Arc<dyn DmaClient> {
        self.cca
            .as_ref()
            .expect("CCA private DMA client is only available when running with --hv cca")
            .private_dma_client
            .clone()
    }

    #[cfg(all(target_os = "linux", not(guest_arch = "aarch64")))]
    pub fn cca_private_dma_client(&self) -> Arc<dyn DmaClient> {
        panic!("CCA private DMA client is only available on aarch64")
    }

    pub async fn new(driver: DefaultDriver, opts: Options) -> anyhow::Result<Self> {
        #[cfg(guest_arch = "x86_64")]
        let processor_topology = TopologyBuilder::new_x86()
            .x2apic(vm_topology::processor::x86::X2ApicState::Supported)
            .build(1)
            .context("failed to build processor topology")?;

        #[cfg(guest_arch = "aarch64")]
        let processor_topology =
            TopologyBuilder::new_aarch64(vm_topology::processor::arch::Aarch64PlatformConfig {
                gic_distributor_base: 0xff000000,
                gic_version: GicVersion::V3 {
                    redistributors_base: 0xff020000,
                },
                gic_msi: vm_topology::processor::aarch64::GicMsiController::None,
                pmu_gsiv: None,
                virt_timer_ppi: 20, // DEFAULT_VIRT_TIMER_PPI
                gic_nr_irqs: 256,
            })
            .build(1)
            .context("failed to build processor topology")?;

        let default_memory_mb = if opts.linux_kernel.is_some() { 192 } else { 4 };
        let ram_size = opts
            .memory_mb
            .unwrap_or(default_memory_mb)
            .checked_mul(1024 * 1024)
            .context("guest RAM size overflowed")?;

        #[cfg_attr(
            not(all(target_os = "linux", guest_arch = "aarch64")),
            expect(unused_mut)
        )]
        let mut memory_layout =
            MemoryLayout::new(ram_size, &[], &[], &[], None).context("bad memory layout")?;
        #[cfg(all(target_os = "linux", guest_arch = "aarch64"))]
        let cca = cca::build(&opts, &mut memory_layout, ram_size)?;

        Ok(Self {
            driver,
            opts,
            processor_topology,
            memory_layout,
            #[cfg(all(target_os = "linux", guest_arch = "aarch64"))]
            cca,
        })
    }

    pub async fn for_each_test(
        &mut self,
        mut f: impl AsyncFnMut(&mut RunContext<'_>, &load::TestInfo) -> anyhow::Result<TestResult>,
    ) -> anyhow::Result<()> {
        let tests = if let Some(tmk_path) = self.opts.tmk.as_ref() {
            let tmk = fs_err::File::open(tmk_path).context("failed to open tmk")?;
            let available_tests = load::enumerate_tests(&tmk)?;
            if self.opts.tests.is_empty() {
                available_tests
            } else {
                self.opts
                    .tests
                    .iter()
                    .map(|name| {
                        available_tests
                            .iter()
                            .find(|test| test.name == *name)
                            .cloned()
                            .with_context(|| format!("test {} not found", name))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?
            }
        } else {
            vec![load::TestInfo {
                name: "cca_linux_virtual_timer_irq".into(),
                index: 0,
            }]
        };
        let mut success = true;
        for test in &tests {
            tracing::info!(target: "test", name = test.name, "test started");

            let mut vmtime_keeper = VmTimeKeeper::new(&self.driver, VmTime::from_100ns(0));
            let vmtime_source = vmtime_keeper.builder().build(&self.driver).await.unwrap();
            let mut ctx = RunContext {
                state: self,
                vmtime_source: &vmtime_source,
            };

            vmtime_keeper.start().await;

            let r = f(&mut ctx, test)
                .await
                .with_context(|| format!("failed to run test {}", test.name))?;

            vmtime_keeper.stop().await;

            match r {
                TestResult::Passed => {
                    tracing::info!(target: "test", name = test.name, "test passed");
                }
                TestResult::Failed => {
                    tracing::error!(target: "test", name = test.name, reason = "explicit failure", "test failed");
                    success = false;
                }
                TestResult::Faulted {
                    vp_index,
                    reason,
                    regs,
                } => {
                    tracing::error!(
                        target: "test",
                        name = test.name,
                        vp_index = vp_index.index(),
                        reason,
                        regs = format_args!("{:#x?}", regs),
                        "test failed"
                    );
                    success = false;
                }
            }
        }
        if !success {
            anyhow::bail!("some tests failed");
        }
        Ok(())
    }
}

impl RunContext<'_> {
    pub async fn run(
        &mut self,
        guest_memory: &GuestMemory,
        caps: &PartitionCapabilities,
        test: &load::TestInfo,
        #[cfg(guest_arch = "aarch64")] control_gic: Option<Arc<dyn virt::irqcon::ControlGic>>,
        start_vp: impl AsyncFnOnce(&mut Self, RunnerBuilder) -> anyhow::Result<()>,
    ) -> anyhow::Result<TestResult> {
        let (event_send, mut event_recv) = mesh::channel();

        #[cfg(guest_arch = "aarch64")]
        let gic = Arc::new(PlatformGic::new(&self.state.processor_topology)?);
        #[cfg(guest_arch = "aarch64")]
        let serial = Arc::new(SerialDevice::new(
            self.state.opts.linux_success_marker.clone(),
            self.state.opts.linux_serial_raw,
            control_gic.as_ref(),
        ));
        #[cfg(guest_arch = "aarch64")]
        if self.state.opts.linux_kernel.is_some() {
            serial.start_input_reader();
        }

        #[cfg(guest_arch = "x86_64")]
        let regs = {
            let tmk_path = self.state.opts.tmk.as_ref().expect("validated by Options");
            let tmk = fs_err::File::open(tmk_path).context("failed to open tmk")?;
            load::load_x86(
                &self.state.memory_layout,
                guest_memory,
                &self.state.processor_topology,
                caps,
                &tmk,
                test,
            )?
        };

        #[cfg(guest_arch = "aarch64")]
        let regs = if let Some(kernel_path) = self.state.opts.linux_kernel.as_ref() {
            let initrd_path = self
                .state
                .opts
                .linux_initrd
                .as_ref()
                .expect("validated by Options");
            let mut kernel =
                fs_err::File::open(kernel_path).context("failed to open Linux Image")?;
            let mut initrd =
                fs_err::File::open(initrd_path).context("failed to open Linux initramfs")?;
            load::load_linux_aarch64(
                &self.state.memory_layout,
                guest_memory,
                &self.state.processor_topology,
                caps,
                &mut kernel,
                &mut initrd,
                &self.state.opts.linux_cmdline,
            )?
        } else {
            let tmk_path = self.state.opts.tmk.as_ref().expect("validated by Options");
            let tmk = fs_err::File::open(tmk_path).context("failed to open tmk")?;
            load::load_aarch64(
                &self.state.memory_layout,
                guest_memory,
                &self.state.processor_topology,
                caps,
                &tmk,
                test,
            )?
        };

        start_vp(
            self,
            RunnerBuilder::new(
                VpIndex::BSP,
                Arc::clone(&regs),
                guest_memory.clone(),
                event_send.clone(),
                #[cfg(guest_arch = "aarch64")]
                gic,
                #[cfg(guest_arch = "aarch64")]
                serial,
            ),
        )
        .await?;

        let event = event_recv.next().await.unwrap();
        let r = match event {
            VpEvent::TestComplete { success } => {
                if success {
                    TestResult::Passed
                } else {
                    TestResult::Failed
                }
            }
            VpEvent::Halt {
                vp_index,
                reason,
                regs,
            } => TestResult::Faulted {
                vp_index,
                reason,
                regs,
            },
        };

        Ok(r)
    }
}

enum VpEvent {
    TestComplete {
        success: bool,
    },
    Halt {
        vp_index: VpIndex,
        reason: String,
        regs: Option<Box<virt::vp::Registers>>,
    },
}

struct IoHandler<'a> {
    guest_memory: &'a GuestMemory,
    event_send: &'a mesh::Sender<VpEvent>,
    stop: &'a StopVpSource,
    #[cfg(guest_arch = "aarch64")]
    gic: &'a PlatformGic,
    #[cfg(guest_arch = "aarch64")]
    serial: &'a SerialDevice,
}

fn widen(d: &[u8]) -> u64 {
    let mut v = [0; 8];
    v[..d.len()].copy_from_slice(d);
    u64::from_ne_bytes(v)
}

impl CpuIo for IoHandler<'_> {
    fn is_mmio(&self, address: u64) -> bool {
        if address == COMMAND_ADDRESS {
            return true;
        }

        #[cfg(guest_arch = "aarch64")]
        {
            self.gic.contains(address) || (UART_BASE..UART_BASE + UART_SIZE).contains(&address)
        }
        #[cfg(not(guest_arch = "aarch64"))]
        {
            false
        }
    }

    fn acknowledge_pic_interrupt(&self) -> Option<u8> {
        None
    }

    fn handle_eoi(&self, irq: u32) {
        tracing::info!(irq, "eoi");
    }

    async fn read_mmio(&self, vp: VpIndex, address: u64, data: &mut [u8]) {
        #[cfg(guest_arch = "aarch64")]
        {
            if self.gic.read(address, data) {
                return;
            }
            if self.read_uart(address, data) {
                return;
            }
        }
        tracing::info!(vp = vp.index(), address, "read mmio");
        data.fill(!0);
    }

    async fn write_mmio(&self, vp: VpIndex, address: u64, data: &[u8]) {
        if address == COMMAND_ADDRESS {
            let p = widen(data);
            let r = self.handle_command(p);
            if let Err(e) = r {
                tracing::error!(
                    error = e.as_ref() as &dyn std::error::Error,
                    p,
                    "failed to handle command"
                );
            }
            return;
        }

        #[cfg(guest_arch = "aarch64")]
        {
            if self.gic.write(address, data) {
                return;
            }
            if self.write_uart(address, data) {
                return;
            }
        }
        tracing::info!(vp = vp.index(), address, data = widen(data), "write mmio");
    }

    async fn read_io(&self, vp: VpIndex, port: u16, data: &mut [u8]) {
        tracing::info!(vp = vp.index(), port, "read io");
        data.fill(!0);
    }

    async fn write_io(&self, vp: VpIndex, port: u16, data: &[u8]) {
        tracing::info!(vp = vp.index(), port, data = widen(data), "write io");
    }

    #[track_caller]
    fn fatal_error(&self, error: Box<dyn std::error::Error + Send + Sync>) -> virt::VpHaltReason {
        tracing::error!(
            err = error.as_ref() as &dyn std::error::Error,
            "fatal error"
        );
        virt::VpHaltReason::TripleFault { vtl: Vtl::Vtl0 }
    }
}

impl IoHandler<'_> {
    #[cfg(guest_arch = "aarch64")]
    fn read_uart(&self, address: u64, data: &mut [u8]) -> bool {
        let Some(offset) = address.checked_sub(UART_BASE) else {
            return false;
        };
        if offset >= UART_SIZE {
            return false;
        }

        let (value, interrupt_update) = {
            let mut serial = self.serial.state.lock().expect("serial mutex poisoned");
            let value = match offset {
                // PL011_DR
                0x00 => u32::from(serial.input.pop_front().unwrap_or(0)),
                // PL011_FR: TX is always empty; RX is empty when no input is queued.
                0x18 => 0x80 | if serial.input.is_empty() { 0x10 } else { 0 },
                // PL011_IMSC
                0x38 => serial.interrupt_mask,
                // PL011_RIS
                0x3c => {
                    if serial.input.is_empty() {
                        0
                    } else {
                        PL011_INT_RX | PL011_INT_RT
                    }
                }
                // PL011_MIS
                0x40 => {
                    if serial.input.is_empty() {
                        0
                    } else {
                        serial.interrupt_mask & (PL011_INT_RX | PL011_INT_RT)
                    }
                }
                _ => 0,
            };
            let update = SerialDevice::update_interrupt(&mut serial);
            (value, update)
        };
        self.serial.apply_interrupt_update(interrupt_update);

        data.fill(0);
        let value = value.to_ne_bytes();
        let len = data.len().min(value.len());
        data[..len].copy_from_slice(&value[..len]);
        true
    }

    #[cfg(guest_arch = "aarch64")]
    fn write_uart(&self, address: u64, data: &[u8]) -> bool {
        let Some(offset) = address.checked_sub(UART_BASE) else {
            return false;
        };
        if offset >= UART_SIZE {
            return false;
        }

        if offset == 0x38 {
            let update = {
                let mut serial = self.serial.state.lock().expect("serial mutex poisoned");
                serial.interrupt_mask = widen(data) as u32;
                SerialDevice::update_interrupt(&mut serial)
            };
            self.serial.apply_interrupt_update(update);
            return true;
        }

        if offset == 0x44 {
            let update = {
                let mut serial = self.serial.state.lock().expect("serial mutex poisoned");
                SerialDevice::update_interrupt(&mut serial)
            };
            self.serial.apply_interrupt_update(update);
            return true;
        }

        if offset != 0x00 {
            return true;
        }

        let Some(&byte) = data.first() else {
            return true;
        };

        let mut serial = self.serial.state.lock().expect("serial mutex poisoned");
        if byte == b'\r' {
            return true;
        }
        if serial.raw_output {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&[byte]);
            if serial.raw_interactive || byte == b'\n' {
                let _ = stdout.flush();
            }
        }
        if byte != b'\n' && serial.line.len() < 4096 {
            serial.line.push(char::from(byte));
            return true;
        }

        let line = std::mem::take(&mut serial.line);
        let success = !serial.completed
            && serial
                .success_marker
                .as_deref()
                .is_some_and(|marker| line.contains(marker));
        if success {
            serial.completed = true;
        }
        let raw_output = serial.raw_output;
        drop(serial);

        if !line.is_empty() && !raw_output {
            tracing::info!(target: "vtl0", message = line);
        }
        if success {
            self.event_send
                .send(VpEvent::TestComplete { success: true });
            self.stop.stop();
        }
        true
    }

    fn read_str(&self, s: tmk_protocol::StrDescriptor) -> anyhow::Result<String> {
        let mut buf = vec![0; s.len as usize];
        self.guest_memory
            .read_at(s.gpa, &mut buf)
            .context("failed to read string")?;
        String::from_utf8(buf).context("string not utf-8")
    }

    fn handle_command(&self, gpa: u64) -> anyhow::Result<()> {
        let buf = self
            .guest_memory
            .read_plain::<[u8; size_of::<tmk_protocol::Command>()]>(gpa)
            .context("failed to read command")?;
        let cmd = tmk_protocol::Command::try_read_from_bytes(&buf)
            .ok()
            .context("bad command")?;
        match cmd {
            tmk_protocol::Command::Log(s) => {
                let message = self.read_str(s)?;
                tracing::info!(target: "tmk", message);
            }
            tmk_protocol::Command::Panic {
                message,
                filename,
                line,
            } => {
                let message = self.read_str(message)?;
                let location = if filename.len > 0 {
                    Some(format!("{}:{}", self.read_str(filename)?, line))
                } else {
                    None
                };
                tracing::error!(target: "tmk", location, panic = message);
                self.event_send
                    .send(VpEvent::TestComplete { success: false });
                self.stop.stop();
            }
            tmk_protocol::Command::Complete { success } => {
                self.event_send.send(VpEvent::TestComplete { success });
                self.stop.stop();
            }
        }
        Ok(())
    }
}

pub struct RunnerBuilder {
    vp_index: VpIndex,
    regs: Arc<virt::InitialRegs>,
    guest_memory: GuestMemory,
    event_send: mesh::Sender<VpEvent>,
    #[cfg(guest_arch = "aarch64")]
    gic: Arc<PlatformGic>,
    #[cfg(guest_arch = "aarch64")]
    serial: Arc<SerialDevice>,
}

impl RunnerBuilder {
    fn new(
        vp_index: VpIndex,
        regs: Arc<virt::InitialRegs>,
        guest_memory: GuestMemory,
        event_send: mesh::Sender<VpEvent>,
        #[cfg(guest_arch = "aarch64")] gic: Arc<PlatformGic>,
        #[cfg(guest_arch = "aarch64")] serial: Arc<SerialDevice>,
    ) -> Self {
        Self {
            vp_index,
            regs,
            guest_memory,
            event_send,
            #[cfg(guest_arch = "aarch64")]
            gic,
            #[cfg(guest_arch = "aarch64")]
            serial,
        }
    }

    pub fn build<P: Processor>(&mut self, mut vp: P) -> anyhow::Result<Runner<'_, P>> {
        {
            let mut state = vp.access_state(Vtl::Vtl0);
            #[cfg(guest_arch = "x86_64")]
            {
                let virt::x86::X86InitialRegs {
                    registers,
                    mtrrs,
                    pat,
                } = self.regs.as_ref();
                state.set_registers(registers)?;
                state.set_mtrrs(mtrrs)?;
                state.set_pat(pat)?;
            }
            #[cfg(guest_arch = "aarch64")]
            {
                let virt::aarch64::Aarch64InitialRegs {
                    registers,
                    system_registers,
                } = self.regs.as_ref();
                state.set_registers(registers)?;
                state.set_system_registers(system_registers)?;
            }
            state.commit()?;
        }
        Ok(Runner {
            vp,
            vp_index: self.vp_index,
            guest_memory: &self.guest_memory,
            event_send: &self.event_send,
            #[cfg(guest_arch = "aarch64")]
            gic: &self.gic,
            #[cfg(guest_arch = "aarch64")]
            serial: &self.serial,
        })
    }
}

pub struct Runner<'a, P> {
    vp: P,
    vp_index: VpIndex,
    guest_memory: &'a GuestMemory,
    event_send: &'a mesh::Sender<VpEvent>,
    #[cfg(guest_arch = "aarch64")]
    gic: &'a PlatformGic,
    #[cfg(guest_arch = "aarch64")]
    serial: &'a SerialDevice,
}

impl<P: Processor> Runner<'_, P> {
    pub async fn run_vp(&mut self) {
        let stop = StopVpSource::new();
        let Err(err) = self
            .vp
            .run_vp(
                stop.checker(),
                &IoHandler {
                    guest_memory: self.guest_memory,
                    event_send: self.event_send,
                    stop: &stop,
                    #[cfg(guest_arch = "aarch64")]
                    gic: self.gic,
                    #[cfg(guest_arch = "aarch64")]
                    serial: self.serial,
                },
            )
            .await;
        let regs = self
            .vp
            .access_state(Vtl::Vtl0)
            .registers()
            .map(Box::new)
            .ok();
        self.event_send.send(VpEvent::Halt {
            vp_index: self.vp_index,
            reason: format!("{:?}", err),
            regs,
        });
    }
}

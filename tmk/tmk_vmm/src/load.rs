// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Support for loading a TMK into VM memory.

use anyhow::Context as _;
use fs_err::File;
use guestmem::GuestMemory;
use hvdef::Vtl;
use loader::importer::GuestArch;
use loader::importer::ImageLoad;
#[cfg(guest_arch = "x86_64")]
use loader::importer::X86Register;
#[cfg(guest_arch = "aarch64")]
use loader::linux::InitrdAddressType;
#[cfg(guest_arch = "aarch64")]
use loader::linux::InitrdConfig;
use object::Endianness;
use object::Object;
use object::ObjectSection;
use object::ObjectSegment as _;
use std::fmt::Debug;
use std::sync::Arc;
use virt::VpIndex;
use vm_topology::memory::MemoryLayout;
use vm_topology::processor::ProcessorTopology;
#[cfg(guest_arch = "aarch64")]
use vm_topology::processor::aarch64::Aarch64Topology;
#[cfg(guest_arch = "x86_64")]
use vm_topology::processor::x86::X86Topology;
use zerocopy::FromBytes as _;
#[cfg(guest_arch = "x86_64")]
use zerocopy::FromZeros;
use zerocopy::IntoBytes;

/// Loads a TMK, returning the initial registers for the BSP.
#[cfg(guest_arch = "x86_64")]
pub fn load_x86(
    memory_layout: &MemoryLayout,
    guest_memory: &GuestMemory,
    processor_topology: &ProcessorTopology<X86Topology>,
    caps: &virt::x86::X86PartitionCapabilities,
    tmk: &File,
    test: &TestInfo,
) -> anyhow::Result<Arc<virt::x86::X86InitialRegs>> {
    let mut loader = vm_loader::Loader::new(guest_memory.clone(), memory_layout, Vtl::Vtl0);
    let load_info = load_common(None, &mut loader, tmk, test)?;

    let page_table_base = load_info.next_available_address;
    let mut page_table_work_buffer: Vec<page_table::x64::PageTable> =
        vec![page_table::x64::PageTable::new_zeroed(); page_table::x64::PAGE_TABLE_MAX_COUNT];
    let mut page_tables: Vec<u8> = vec![0; page_table::x64::PAGE_TABLE_MAX_BYTES];
    let page_table_builder = page_table::x64::IdentityMapBuilder::new(
        page_table_base,
        page_table::IdentityMapSize::Size4Gb,
        page_table_work_buffer.as_mut_slice(),
        page_tables.as_mut_slice(),
    )?;
    let page_tables = page_table_builder.build();
    loader
        .import_pages(
            page_table_base >> 12,
            page_tables.len() as u64 >> 12,
            "page_tables",
            loader::importer::BootPageAcceptance::Exclusive,
            page_tables,
        )
        .context("failed to import page tables")?;

    let gdt_base = page_table_base + page_tables.len() as u64;
    loader::common::import_default_gdt(&mut loader, gdt_base >> 12)
        .context("failed to import gdt")?;

    let mut import_reg = |reg| {
        loader
            .import_vp_register(reg)
            .context("failed to set register")
    };
    import_reg(X86Register::Cr0(x86defs::X64_CR0_PG | x86defs::X64_CR0_PE))?;
    import_reg(X86Register::Cr3(page_table_base))?;
    import_reg(X86Register::Cr4(x86defs::X64_CR4_PAE))?;
    import_reg(X86Register::Efer(
        x86defs::X64_EFER_SCE
            | x86defs::X64_EFER_LME
            | x86defs::X64_EFER_LMA
            | x86defs::X64_EFER_NXE,
    ))?;
    import_reg(X86Register::Rip(load_info.entrypoint))?;
    import_reg(X86Register::Rsi(load_info.param))?;

    let regs = vm_loader::initial_regs::x86_initial_regs(
        &loader.initial_regs(),
        caps,
        &processor_topology.vp_arch(VpIndex::BSP),
    );
    Ok(regs)
}

#[cfg(guest_arch = "aarch64")]
pub fn load_aarch64(
    memory_layout: &MemoryLayout,
    guest_memory: &GuestMemory,
    processor_topology: &ProcessorTopology<Aarch64Topology>,
    caps: &virt::aarch64::Aarch64PartitionCapabilities,
    tmk: &File,
    test: &TestInfo,
) -> anyhow::Result<Arc<virt::aarch64::Aarch64InitialRegs>> {
    let mut loader = vm_loader::Loader::new(guest_memory.clone(), memory_layout, Vtl::Vtl0);
    let load_info = load_common(
        Some(memory_layout.ram()[0].range.start()),
        &mut loader,
        tmk,
        test,
    )?;

    let mut import_reg = |reg| {
        loader
            .import_vp_register(reg)
            .context("failed to set register")
    };

    import_reg(loader::importer::Aarch64Register::Pc(load_info.entrypoint))?;
    import_reg(loader::importer::Aarch64Register::X0(load_info.param))?;
    let regs = vm_loader::initial_regs::aarch64_initial_regs(
        &loader.initial_regs(),
        caps,
        &processor_topology.vp_arch(VpIndex::BSP),
    );

    Ok(regs)
}

/// Direct-boots an AArch64 Linux Image with a minimal CCA test platform DT.
#[cfg(guest_arch = "aarch64")]
pub fn load_linux_aarch64(
    memory_layout: &MemoryLayout,
    guest_memory: &GuestMemory,
    processor_topology: &ProcessorTopology<Aarch64Topology>,
    caps: &virt::aarch64::Aarch64PartitionCapabilities,
    kernel: &mut File,
    initrd: &mut File,
    cmdline: &str,
) -> anyhow::Result<Arc<virt::aarch64::Aarch64InitialRegs>> {
    const INITRD_OFFSET: u64 = 16 << 20;

    let memory_start = memory_layout
        .ram()
        .first()
        .context("direct Linux boot requires RAM")?
        .range
        .start();
    let initrd_size = initrd
        .metadata()
        .context("failed to inspect Linux initramfs")?
        .len();
    let initrd_start = memory_start
        .checked_add(INITRD_OFFSET)
        .context("Linux initramfs address overflowed")?;
    let initrd_end = initrd_start
        .checked_add(initrd_size)
        .context("Linux initramfs range overflowed")?;
    let kernel_minimum_start_address = initrd_end
        .checked_add(0x1f_ffff)
        .context("Linux kernel address overflowed")?
        & !0x1f_ffff;

    let device_tree = build_linux_dt(
        memory_layout,
        processor_topology,
        cmdline,
        initrd_start,
        initrd_end,
    )
    .context("failed to build Linux device tree")?;

    let mut loader = vm_loader::Loader::new(guest_memory.clone(), memory_layout, Vtl::Vtl0);
    let load_info = loader::linux::load_kernel_and_initrd_arm64(
        &mut loader,
        kernel,
        kernel_minimum_start_address,
        Some(InitrdConfig {
            initrd_address: InitrdAddressType::Address(initrd_start),
            initrd,
            size: initrd_size,
        }),
        Some(&device_tree),
    )
    .context("failed to load AArch64 Linux Image")?;
    loader::linux::set_direct_boot_registers_arm64(&mut loader, &load_info)
        .context("failed to set AArch64 Linux boot registers")?;

    Ok(vm_loader::initial_regs::aarch64_initial_regs(
        &loader.initial_regs(),
        caps,
        &processor_topology.vp_arch(VpIndex::BSP),
    ))
}

#[cfg(guest_arch = "aarch64")]
fn build_linux_dt(
    memory_layout: &MemoryLayout,
    processor_topology: &ProcessorTopology<Aarch64Topology>,
    cmdline: &str,
    initrd_start: u64,
    initrd_end: u64,
) -> Result<Vec<u8>, fdt::builder::Error> {
    use vm_topology::processor::aarch64::GicVersion;

    const UART_BASE: u64 = 0xeffe_c000;
    const UART_SPI: u32 = 1;
    const PHANDLE_GIC: u32 = 1;
    const GIC_SPI: u32 = 0;
    const GIC_PPI: u32 = 1;
    const IRQ_TYPE_LEVEL_HIGH: u32 = 4;
    const IRQ_TYPE_LEVEL_LOW: u32 = 8;

    let GicVersion::V3 {
        redistributors_base,
    } = processor_topology.gic_version()
    else {
        unreachable!("CCA Linux prototype requires GICv3")
    };

    let mut buffer = vec![0u8; 0x1_0000];
    let mut builder = fdt::builder::Builder::new(fdt::builder::BuilderConfig {
        blob_buffer: &mut buffer,
        string_table_cap: 512,
        memory_reservations: &[],
    })?;

    let p_address_cells = builder.add_string("#address-cells")?;
    let p_size_cells = builder.add_string("#size-cells")?;
    let p_compatible = builder.add_string("compatible")?;
    let p_device_type = builder.add_string("device_type")?;
    let p_reg = builder.add_string("reg")?;
    let p_status = builder.add_string("status")?;
    let p_interrupt_cells = builder.add_string("#interrupt-cells")?;
    let p_interrupt_controller = builder.add_string("interrupt-controller")?;
    let p_interrupt_parent = builder.add_string("interrupt-parent")?;
    let p_interrupts = builder.add_string("interrupts")?;
    let p_interrupt_names = builder.add_string("interrupt-names")?;
    let p_always_on = builder.add_string("always-on")?;
    let p_phandle = builder.add_string("phandle")?;
    let p_bootargs = builder.add_string("bootargs")?;
    let p_stdout_path = builder.add_string("stdout-path")?;
    let p_initrd_start = builder.add_string("linux,initrd-start")?;
    let p_initrd_end = builder.add_string("linux,initrd-end")?;
    let p_current_speed = builder.add_string("current-speed")?;

    let mut root = builder
        .start_node("")?
        .add_u32(p_address_cells, 2)?
        .add_u32(p_size_cells, 2)?
        .add_u32(p_interrupt_parent, PHANDLE_GIC)?
        .add_str(p_compatible, "microsoft,openvmm-cca-prototype")?;

    let cpu = root
        .start_node("cpus")?
        .add_u32(p_address_cells, 1)?
        .add_u32(p_size_cells, 0)?
        .start_node("cpu@0")?
        .add_str(p_device_type, "cpu")?
        .add_str(p_compatible, "arm,armv8")?
        .add_u32(p_reg, 0)?
        .add_str(p_status, "okay")?
        .end_node()?
        .end_node()?;
    root = cpu;

    for entry in memory_layout.ram() {
        root = root
            .start_node(format!("memory@{:x}", entry.range.start()).as_str())?
            .add_str(p_device_type, "memory")?
            .add_u64_array(p_reg, &[entry.range.start(), entry.range.len()])?
            .end_node()?;
    }

    root = root
        .start_node(format!("intc@{:x}", processor_topology.gic_distributor_base()).as_str())?
        .add_str(p_compatible, "arm,gic-v3")?
        .add_u64_array(
            p_reg,
            &[
                processor_topology.gic_distributor_base(),
                aarch64defs::GIC_DISTRIBUTOR_SIZE,
                redistributors_base,
                aarch64defs::GIC_REDISTRIBUTOR_SIZE * u64::from(processor_topology.vp_count()),
            ],
        )?
        .add_u32(p_address_cells, 2)?
        .add_u32(p_size_cells, 2)?
        .add_u32(p_interrupt_cells, 3)?
        .add_null(p_interrupt_controller)?
        .add_u32(p_phandle, PHANDLE_GIC)?
        .end_node()?;

    let timer_ppi = processor_topology.virt_timer_ppi();
    assert!((16..32).contains(&timer_ppi));
    root = root
        .start_node("timer")?
        .add_str(p_compatible, "arm,armv8-timer")?
        .add_str(p_interrupt_names, "virt")?
        .add_u32_array(p_interrupts, &[GIC_PPI, timer_ppi - 16, IRQ_TYPE_LEVEL_LOW])?
        .add_null(p_always_on)?
        .end_node()?;

    root = root
        .start_node(format!("uart@{UART_BASE:x}").as_str())?
        .add_str(p_compatible, "arm,sbsa-uart")?
        .add_u64_array(p_reg, &[UART_BASE, 0x1000])?
        .add_u32_array(p_interrupts, &[GIC_SPI, UART_SPI, IRQ_TYPE_LEVEL_HIGH])?
        .add_u32(p_current_speed, 115200)?
        .add_str(p_status, "okay")?
        .end_node()?;

    root = root
        .start_node("chosen")?
        .add_str(p_bootargs, cmdline)?
        .add_str(p_stdout_path, "/uart@effec000")?
        .add_u64(p_initrd_start, initrd_start)?
        .add_u64(p_initrd_end, initrd_end)?
        .end_node()?;

    let size = root.end_node()?.build(0)?;
    buffer.truncate(size);
    Ok(buffer)
}

fn load_common<R: Debug + GuestArch>(
    offset_addr: Option<u64>,
    loader: &mut vm_loader::Loader<'_, R>,
    tmk: &File,
    test: &TestInfo,
) -> anyhow::Result<LoadInfo> {
    let load_info = loader::elf::load_static_elf(
        loader,
        &mut &*tmk,
        0,
        offset_addr.unwrap_or(0x200000),
        false,
        loader::importer::BootPageAcceptance::Exclusive,
        "tmk",
    )
    .context("failed to load tmk")?;

    let start_input = tmk_protocol::StartInput {
        command: crate::run::COMMAND_ADDRESS,
        test_index: test.index,
    };

    let start_input_addr = load_info.next_available_address;

    loader.import_pages(
        start_input_addr >> 12,
        1,
        "start_input",
        loader::importer::BootPageAcceptance::Exclusive,
        start_input.as_bytes(),
    )?;

    Ok(LoadInfo {
        entrypoint: load_info.entrypoint,
        param: start_input_addr,
        next_available_address: start_input_addr + 0x1000,
    })
}

struct LoadInfo {
    entrypoint: u64,
    param: u64,
    #[cfg_attr(guest_arch = "aarch64", expect(dead_code))]
    next_available_address: u64,
}

#[derive(Clone)]
pub struct TestInfo {
    pub name: String,
    pub index: u64,
}

/// Enumerate the tests from a TMK binary.
///
/// The test definitions are stored as an array of
/// [`tmk_protocol::TestDescriptor64`] in the "tmk_tests" section of the binary.
pub fn enumerate_tests(tmk: &File) -> anyhow::Result<Vec<TestInfo>> {
    let reader = object::ReadCache::new(tmk);
    let file: object::read::elf::ElfFile64<'_, Endianness, _> =
        object::read::elf::ElfFile::parse(&reader).context("failed to parse TMK")?;

    let mut relocs = file
        .dynamic_relocations()
        .context("failed to find dynamic relocations")?
        .collect::<Vec<_>>();
    relocs.sort_by_key(|&(a, _)| a);

    // Relocate address `v` that was loaded from address `addr`.
    let reloc = |addr, v: u64| {
        let r = relocs.binary_search_by_key(&addr, |&(a, _)| a);
        match r {
            Ok(i) => {
                let reloc = &relocs[i].1;
                v.wrapping_add_signed(reloc.addend())
            }
            Err(_) => v,
        }
    };

    let section = file
        .section_by_name("tmk_tests")
        .context("failed to find tmk_tests section")?;
    let data = section.data()?;
    let descriptors = <[tmk_protocol::TestDescriptor64]>::ref_from_bytes(data)
        .ok()
        .context("failed to parse tmk_tests section")?;
    let mut tests = Vec::with_capacity(descriptors.len());
    for (i, t) in descriptors.iter().enumerate() {
        let name_address = reloc(
            section.address() + (i * size_of::<tmk_protocol::TestDescriptor64>()) as u64,
            t.name,
        );
        let name = file
            .segments()
            .find_map(|s| s.data_range(name_address, t.name_len).transpose())
            .context("failed to find name for test")?
            .context("failed to parse tmk")?;
        let name = core::str::from_utf8(name).context("failed to parse test name")?;

        tests.push(TestInfo {
            name: name.to_string(),
            index: i as u64,
        });
    }

    Ok(tests)
}

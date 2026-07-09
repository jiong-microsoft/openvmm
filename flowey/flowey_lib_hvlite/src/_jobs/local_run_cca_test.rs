// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Run OpenVMM CCA tests. Now we run them using emulator, code can be tweaked
//! to support running tests on native hardware platform.
use crate::build_openvmm::OpenvmmFeature;
use crate::common::CommonArch;
use crate::common::CommonPlatform;
use crate::common::CommonProfile;
use crate::common::CommonTriple;
use flowey::node::prelude::*;
use std::collections::BTreeSet;

flowey_request! {
    pub struct Params {
        pub test_root: PathBuf,
        pub build_only: bool,
        pub pause_before_start_tmk: bool,
        pub interactive_vtl0_shell: bool,
        pub done: WriteVar<SideEffect>,
    }
}

new_simple_flow_node!(struct Node);

const ENV_CCA_TEST_ROOT: &str = "OPENVMM_CCA_TEST_ROOT";
const ENV_CCA_OPENVMM: &str = "OPENVMM_CCA_OPENVMM";
const ENV_CCA_UEFI_IGVM: &str = "OPENVMM_CCA_UEFI_IGVM";
const ENV_CCA_TMK_VMM: &str = "OPENVMM_CCA_TMK_VMM";
const ENV_CCA_PAUSE_BEFORE_START_TMK: &str = "OPENVMM_CCA_PAUSE_BEFORE_START_TMK";
const ENV_CCA_INTERACTIVE_VTL0_SHELL: &str = "OPENVMM_CCA_INTERACTIVE_VTL0_SHELL";

impl SimpleFlowNode for Node {
    type Request = Params;

    fn imports(ctx: &mut ImportCtx<'_>) {
        ctx.import::<crate::build_openvmm::Node>();
        ctx.import::<crate::build_tmk_vmm::Node>();
        ctx.import::<crate::build_uefi_igvm::Node>();
    }

    fn process_request(request: Self::Request, ctx: &mut NodeCtx<'_>) -> anyhow::Result<()> {
        let Params {
            test_root,
            build_only,
            pause_before_start_tmk,
            interactive_vtl0_shell,
            done,
        } = request;

        // Generate request to build openvmm for the CCA Realm.
        let openvmm_output = ctx.reqv(|v| crate::build_openvmm::Request {
            params: crate::build_openvmm::OpenvmmBuildParams {
                target: CommonTriple::Common {
                    arch: CommonArch::Aarch64,
                    platform: CommonPlatform::LinuxGnu,
                },
                profile: CommonProfile::Debug,
                features: BTreeSet::from([OpenvmmFeature::VendoredCrypto]),
            },
            version: None,
            openvmm: v,
        });
        let tmk_vmm_output = ctx.reqv(|v| crate::build_tmk_vmm::Request {
            target: CommonTriple::Common {
                arch: CommonArch::Aarch64,
                platform: CommonPlatform::LinuxGnu,
            },
            profile: CommonProfile::Debug,
            tmk_vmm: v,
        });
        let uefi_igvm = ctx.reqv(|v| crate::build_uefi_igvm::Request { igvm: v });

        ctx.emit_rust_step("running cca tests", |ctx| {
            done.claim(ctx);
            let openvmm_output = openvmm_output.claim(ctx);
            let tmk_vmm_output = tmk_vmm_output.claim(ctx);
            let uefi_igvm = uefi_igvm.claim(ctx);
            move |rt| {
                let openvmm_output = rt.read(openvmm_output);
                let tmk_vmm_output = rt.read(tmk_vmm_output);
                let uefi_igvm = rt.read(uefi_igvm);
                let crate::build_openvmm::OpenvmmOutput::LinuxBin {
                    bin: openvmm_bin, ..
                } = openvmm_output
                else {
                    anyhow::bail!("expect Linux openvmm only");
                };
                let crate::build_tmk_vmm::TmkVmmOutput::LinuxBin {
                    bin: tmk_vmm_bin,
                    ..
                } = tmk_vmm_output
                else {
                    anyhow::bail!("expect Linux tmk_vmm only");
                };

                if build_only {
                    log::info!("CCA test artifacts built; skipping Petri test because --build-only was specified");
                    log::info!("openvmm: {}", openvmm_bin.display());
                    log::info!("tmk_vmm: {}", tmk_vmm_bin.display());
                    log::info!("AArch64 UEFI IGVM: {}", uefi_igvm.display());
                    return Ok(());
                }

                let mut cmd = flowey::shell_cmd!(rt, "cargo test -p vmm_tests --test cca")
                    .env(ENV_CCA_TEST_ROOT, &test_root)
                    .env(ENV_CCA_OPENVMM, &openvmm_bin)
                    .env(ENV_CCA_UEFI_IGVM, &uefi_igvm)
                    .env(ENV_CCA_TMK_VMM, &tmk_vmm_bin);

                if pause_before_start_tmk {
                    cmd = cmd.env(ENV_CCA_PAUSE_BEFORE_START_TMK, "1");
                }
                if interactive_vtl0_shell {
                    cmd = cmd.env(ENV_CCA_INTERACTIVE_VTL0_SHELL, "1");
                }

                cmd.run()
                    .with_context(|| "failed to run CCA runtime Petri test")?;

                Ok(())
            }
        });

        Ok(())
    }
}

// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Build an AArch64 IGVM whose payload is UEFI.

use crate::common::CommonArch;
use crate::common::CommonPlatform;
use crate::common::CommonTriple;
use crate::run_cargo_build::BuildProfile;
use flowey::node::prelude::*;
use igvmfilegen_config::ResourceType;
use std::collections::BTreeMap;

flowey_request! {
    pub struct Request {
        pub igvm: WriteVar<PathBuf>,
    }
}

new_simple_flow_node!(struct Node);

impl SimpleFlowNode for Node {
    type Request = Request;

    fn imports(ctx: &mut ImportCtx<'_>) {
        ctx.import::<crate::build_igvmfilegen::Node>();
        ctx.import::<crate::download_uefi_mu_msvm::Node>();
        ctx.import::<crate::git_checkout_openvmm_repo::Node>();
        ctx.import::<crate::run_igvmfilegen::Node>();
    }

    fn process_request(request: Self::Request, ctx: &mut NodeCtx<'_>) -> anyhow::Result<()> {
        let Request { igvm } = request;
        let arch = CommonArch::Aarch64;

        let openvmm_repo = ctx.reqv(crate::git_checkout_openvmm_repo::req::GetRepoDir);
        let manifest = openvmm_repo.map(ctx, |repo| {
            repo.join("vm/loader/manifests/uefi-aarch64.json")
        });

        let uefi =
            ctx.reqv(|v| crate::download_uefi_mu_msvm::Request::GetMsvmFd { arch, msvm_fd: v });
        let resources = uefi.map(ctx, |uefi| BTreeMap::from([(ResourceType::Uefi, uefi)]));

        let host_arch = ctx.arch().try_into()?;
        let igvmfilegen = ctx.reqv(|v| crate::build_igvmfilegen::Request {
            build_params: crate::build_igvmfilegen::IgvmfilegenBuildParams {
                target: CommonTriple::Common {
                    arch: host_arch,
                    platform: CommonPlatform::LinuxGnu,
                },
                profile: BuildProfile::Light,
            },
            igvmfilegen: v,
        });
        let igvmfilegen = igvmfilegen.map(ctx, |output| match output {
            crate::build_igvmfilegen::IgvmfilegenOutput::LinuxBin { bin, dbg: _ } => bin,
            crate::build_igvmfilegen::IgvmfilegenOutput::WindowsBin { exe, pdb: _ } => exe,
        });

        let output = ctx.reqv(|v| crate::run_igvmfilegen::Request {
            igvmfilegen,
            manifest,
            resources,
            disable_secure_avic: false,
            igvm: v,
        });
        output.write_into_with(ctx, igvm, |output| output.igvm_bin);

        Ok(())
    }
}

// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use flowey::node::prelude::ReadVar;
use flowey::pipeline::prelude::*;
use std::path::PathBuf;

/// CCA test flows, including installing, updating CCA emulation environment and run OpenVMM tests
#[derive(clap::Args)]
pub struct CcaTestsCli {
    /// Root directory for holding all CCA test related stuff
    #[clap(long, default_value = "target/cca-test")]
    pub test_root: PathBuf,

    /// Install CCA emulation environment, including downloading emulator and building all needed firmware
    #[clap(long)]
    pub install_emu: bool,

    /// Update CCA emulation environment by rebuilding firmwares, support a few sub-commands
    #[clap(long)]
    pub update_emu: bool,

    /// Build CCA test artifacts without running the Petri test.
    #[clap(long)]
    pub build_only: bool,

    /// Use a local AArch64 mu_msvm MSVM.fd when building the UEFI IGVM.
    #[clap(long, value_name = "PATH")]
    pub custom_uefi: Option<PathBuf>,

    /// Pause at the CCA plane0 shell before running /root/start-tmk.sh.
    #[clap(long)]
    pub pause_before_start_tmk: bool,

    /// Pause at the interactive VTL0 Linux shell and forward terminal commands.
    #[clap(long, conflicts_with = "pause_before_start_tmk")]
    pub interactive_vtl0_shell: bool,

    /// Verbose pipeline output
    #[clap(long)]
    pub verbose: bool,

    #[clap(flatten)]
    pub update_emu_subcmds: CcaTestsUpdateEmuSubCmds,
}

#[derive(clap::Args)]
#[clap(next_help_heading = "--update_emu subcommands")]
pub struct CcaTestsUpdateEmuSubCmds {
    /// Rebuild the plane0 Linux image from the existing source tree.
    #[clap(long)]
    pub rebuild_plane0_linux: bool,

    /// Rebuild the shrinkwrap-generated rootfs image.
    #[clap(long)]
    pub rebuild_rootfs: bool,

    /// Update TF-A to specified revision and rebuild.
    #[clap(long)]
    pub tfa_rev: Option<String>,

    /// Update TF-RMM to specified revision and rebuild.
    #[clap(long)]
    pub tfrmm_rev: Option<String>,

    /// Update plane0 Linux to specified revision and rebuild.
    #[clap(long)]
    pub plane0_linux_rev: Option<String>,
}

impl IntoPipeline for CcaTestsCli {
    fn into_pipeline(self, backend_hint: PipelineBackendHint) -> anyhow::Result<Pipeline> {
        let Self {
            test_root,
            install_emu,
            update_emu,
            build_only,
            custom_uefi,
            pause_before_start_tmk,
            interactive_vtl0_shell,
            verbose,
            update_emu_subcmds:
                CcaTestsUpdateEmuSubCmds {
                    rebuild_plane0_linux,
                    rebuild_rootfs,
                    tfa_rev,
                    tfrmm_rev,
                    plane0_linux_rev,
                },
        } = self;

        let openvmm_repo = flowey_lib_common::git_checkout::RepoSource::ExistingClone(
            ReadVar::from_static(crate::repo_root()),
        );

        // Absolute path is expected across cca_tests infrastructure. Relative
        // paths are resolved from repo root.
        let test_root = if test_root.is_absolute() {
            test_root
        } else {
            crate::repo_root().join(test_root)
        };

        let mut pipeline = Pipeline::new();

        if install_emu {
            let check_job = pipeline
                .new_job(
                    FlowPlatform::host(backend_hint),
                    FlowArch::host(backend_hint),
                    "cca-tests: check existence of emulation environment needed tools",
                )
                .config(flowey_lib_common::install_dist_pkg::Config {
                    interactive: Some(true),
                    skip_update: Some(false),
                })
                .dep_on(
                    |ctx| flowey_lib_hvlite::_jobs::local_check_cca_emu_prereq::Params {
                        done: ctx.new_done_handle(),
                    },
                )
                .finish();

            let install_job = pipeline
                .new_job(
                    FlowPlatform::host(backend_hint),
                    FlowArch::host(backend_hint),
                    "cca-tests: install emulation environment",
                )
                .config(flowey_lib_common::git_checkout::Config {
                    require_local_clones: Some(false),
                })
                .config(flowey_lib_common::install_git::Config {
                    auto_install: Some(true),
                })
                .config(flowey_lib_common::install_dist_pkg::Config {
                    interactive: Some(true),
                    skip_update: Some(false),
                })
                .dep_on(
                    |ctx| flowey_lib_hvlite::_jobs::local_install_cca_emu::Params {
                        test_root: test_root.clone(),
                        openvmm_root: crate::repo_root(),
                        done: ctx.new_done_handle(),
                    },
                )
                .finish();

            pipeline.non_artifact_dep(&install_job, &check_job);
            return Ok(pipeline);
        }

        let update_job = if update_emu {
            Some(
                pipeline
                    .new_job(
                        FlowPlatform::host(backend_hint),
                        FlowArch::host(backend_hint),
                        "cca-tests: update emulation environment",
                    )
                    .dep_on(
                        |ctx| flowey_lib_hvlite::_jobs::local_update_cca_emu::Params {
                            test_root: test_root.clone(),
                            openvmm_root: crate::repo_root(),
                            sub_cmds: flowey_lib_hvlite::_jobs::local_update_cca_emu::SubCmds {
                                rebuild_plane0_linux,
                                rebuild_rootfs,
                                tfa_rev,
                                tfrmm_rev,
                                plane0_linux_rev,
                            },
                            done: ctx.new_done_handle(),
                        },
                    )
                    .finish(),
            )
        } else {
            None
        };

        let mut test_job = pipeline
            .new_job(
                FlowPlatform::host(backend_hint),
                FlowArch::host(backend_hint),
                "cca-tests: run cca tests",
            )
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_versions::Request::Init);

        if let Some(uefi_path) = custom_uefi {
            test_job = test_job.dep_on(move |_| {
                flowey_lib_hvlite::_jobs::cfg_versions::Request::LocalUefi(
                    flowey_lib_hvlite::common::CommonArch::Aarch64,
                    ReadVar::from_static(uefi_path),
                )
            });
        }

        let test_job = test_job
            .dep_on(
                |_| flowey_lib_hvlite::_jobs::cfg_hvlite_reposource::Params {
                    hvlite_repo_source: openvmm_repo.clone(),
                },
            )
            .dep_on(|_| flowey_lib_hvlite::_jobs::cfg_common::Params {
                local_only: Some(flowey_lib_hvlite::_jobs::cfg_common::LocalOnlyParams {
                    interactive: true,
                    auto_install: true,
                    ignore_rust_version: true,
                }),
                verbose: ReadVar::from_static(verbose),
                locked: false,
                deny_warnings: false,
                no_incremental: false,
            })
            .dep_on(|ctx| flowey_lib_hvlite::_jobs::local_run_cca_test::Params {
                test_root: test_root.clone(),
                build_only,
                pause_before_start_tmk,
                interactive_vtl0_shell,
                done: ctx.new_done_handle(),
            })
            .finish();

        // Only add dependency if update_job exists
        if let Some(update_job) = &update_job {
            pipeline.non_artifact_dep(&test_job, update_job);
        }

        Ok(pipeline)
    }
}

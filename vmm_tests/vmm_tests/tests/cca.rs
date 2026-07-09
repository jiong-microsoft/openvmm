// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Test entrypoint for CCA emulation tests.

#![forbid(unsafe_code)]

use anyhow::Context as _;
use nix::sys::termios;
use std::ffi::OsStr;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

const CCA_TEST_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const CCA_VTL0_TIMER_IRQ_MARKER: &str = "CCA_VTL0_TIMER_IRQ_OK";
const CCA_VTL0_SHELL_READY_MARKER: &str = "CCA_VTL0_SHELL_READY";
const CCA_VTL0_SHELL_PROMPT: &str = "/ #";
const CCA_VTL0_SHELL_COMMAND: &str = "printf 'CCA_VTL0_SHELL_%s_OK\\n' COMMAND";
const CCA_TEST_SUCCESS_MARKER: &str = "CCA_VTL0_SHELL_COMMAND_OK";
const CCA_VTL0_INITRAMFS_PATH: &str = "cca/vtl0-initramfs.cpio";
const CCA_PLANE0_PROMPT: &str = "sh-5.2#";
const CCA_START_TMK_COMMAND: &str = "/root/busybox sh /root/start-tmk.sh";
const CCA_PAUSE_BEFORE_START_TMK_ENV: &str = "OPENVMM_CCA_PAUSE_BEFORE_START_TMK";
const CCA_INTERACTIVE_VTL0_SHELL_ENV: &str = "OPENVMM_CCA_INTERACTIVE_VTL0_SHELL";
const CCA_PAUSE_CONTINUE_COMMAND: &str = ".continue";
const CCA_INTERACTIVE_ESCAPE: u8 = 0x1d;
const CCA_OUTPUT_WINDOW_SIZE: usize = 64 * 1024;
const CCA_TEST_FAILURE_MARKERS: &[&str] = &[
    "test failed",
    "some tests failed",
    "[realm-launch][ERROR]",
    "CCA_VTL0_TIMER_SLEEP_FAILED",
    "CCA_VTL0_CONSOLE_SETUP_FAILED",
    "CCA_VTL0_SHELL_EXEC_FAILED",
    "error while loading shared libraries",
    "Internal error: Oops",
    "Kernel panic",
    "Segmentation fault",
    "panicked at",
];
const CCA_VTL0_INIT_SOURCE: &[u8] = include_bytes!("../test_data/cca_vtl0_init.c");
const CCA_VTL0_INIT_SCRIPT: &[u8] = include_bytes!("../test_data/cca_vtl0_init.sh");

struct CcaRuntimeArtifacts {
    shrinkwrap_exe: petri::ResolvedArtifact,
    venv_dir: petri::ResolvedArtifact,
    rootfs_file: petri::ResolvedArtifact,
    e2fsck_bin: petri::ResolvedArtifact,
    resize2fs_bin: petri::ResolvedArtifact,
    openvmm_bin: petri::ResolvedArtifact,
    tmk_vmm_bin: petri::ResolvedArtifact,
    guest_disk: petri::ResolvedArtifact,
    plane0_linux_image: petri::ResolvedArtifact,
    kvmtool_efi: petri::ResolvedArtifact,
    lkvm: petri::ResolvedArtifact,
    uefi_igvm: petri::ResolvedArtifact,
}

impl CcaRuntimeArtifacts {
    fn validate(&self) -> anyhow::Result<()> {
        for (name, path) in self.paths() {
            if !path.exists() {
                anyhow::bail!("{name} points to missing path {}", path.display());
            }

            tracing::info!(artifact = name, path = %path.display(), "resolved CCA runtime artifact");
        }

        Ok(())
    }

    fn paths(&self) -> [(&'static str, &Path); 12] {
        [
            ("cca::SHRINKWRAP", self.shrinkwrap_exe.get()),
            ("cca::VENV", self.venv_dir.get()),
            ("cca::ROOTFS", self.rootfs_file.get()),
            ("cca::E2FSCK", self.e2fsck_bin.get()),
            ("cca::RESIZE2FS", self.resize2fs_bin.get()),
            ("OPENVMM_LINUX_AARCH64", self.openvmm_bin.get()),
            ("tmks::TMK_VMM_LINUX_AARCH64", self.tmk_vmm_bin.get()),
            ("cca::GUEST_DISK", self.guest_disk.get()),
            ("cca::PLANE0_LINUX_IMAGE", self.plane0_linux_image.get()),
            ("cca::KVMTOOL_EFI", self.kvmtool_efi.get()),
            ("cca::LKVM", self.lkvm.get()),
            ("cca::UEFI_IGVM", self.uefi_igvm.get()),
        ]
    }
}

fn resolve_cca_runtime(resolver: &petri::ArtifactResolver<'_>) -> Option<CcaRuntimeArtifacts> {
    Some(CcaRuntimeArtifacts {
        shrinkwrap_exe: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::SHRINKWRAP)
            .erase(),
        venv_dir: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::VENV)
            .erase(),
        rootfs_file: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::ROOTFS)
            .erase(),
        e2fsck_bin: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::E2FSCK)
            .erase(),
        resize2fs_bin: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::RESIZE2FS)
            .erase(),
        openvmm_bin: resolver
            .require(petri_artifacts_vmm_test::artifacts::OPENVMM_LINUX_AARCH64)
            .erase(),
        tmk_vmm_bin: resolver
            .require(petri_artifacts_vmm_test::artifacts::tmks::TMK_VMM_LINUX_AARCH64)
            .erase(),
        guest_disk: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::GUEST_DISK)
            .erase(),
        plane0_linux_image: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::PLANE0_LINUX_IMAGE)
            .erase(),
        kvmtool_efi: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::KVMTOOL_EFI)
            .erase(),
        lkvm: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::LKVM)
            .erase(),
        uefi_igvm: resolver
            .require(petri_artifacts_vmm_test::artifacts::cca::UEFI_IGVM)
            .erase(),
    })
}

fn cca_runtime(
    params: petri::PetriTestParams<'_>,
    artifacts: CcaRuntimeArtifacts,
) -> anyhow::Result<()> {
    artifacts.validate()?;
    let rootfs = prepare_cca_rootfs(&artifacts)?;
    tracing::info!("launching openvmm cca tests...");

    let venv_bin_path = format!(
        "{}:{}",
        artifacts.venv_dir.get().join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    run_shrinkwrap_cca_test(
        artifacts.shrinkwrap_exe.get(),
        artifacts.venv_dir.get(),
        rootfs.path(),
        &venv_bin_path,
        params.logger.log_file("shrinkwrap_stdout")?,
        params.logger.log_file("shrinkwrap_stderr")?,
    )?;

    tracing::info!("openvmm cca tests finished");

    Ok(())
}

struct PreparedCcaRootfs {
    test_dir: tempfile::TempDir,
    rootfs_path: PathBuf,
}

impl PreparedCcaRootfs {
    fn path(&self) -> &Path {
        debug_assert!(self.rootfs_path.starts_with(self.test_dir.path()));
        &self.rootfs_path
    }
}

fn prepare_cca_rootfs(artifacts: &CcaRuntimeArtifacts) -> anyhow::Result<PreparedCcaRootfs> {
    let test_dir = tempfile::tempdir().context("failed to create CCA runtime test directory")?;
    let rootfs_path = test_dir.path().join("rootfs.ext2");
    let start_tmk_path = test_dir.path().join("start-tmk.sh");
    let run_realm_test_path = test_dir.path().join("run_realm_test.sh");
    std::fs::write(
        &start_tmk_path,
        include_bytes!("../test_data/cca_start_tmk.sh"),
    )
    .context("failed to stage the CCA Plane0 launch script")?;
    std::fs::write(
        &run_realm_test_path,
        include_bytes!("../test_data/cca_run_realm_test.sh"),
    )
    .context("failed to stage the CCA realm launch script")?;
    for script in [&start_tmk_path, &run_realm_test_path] {
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to make {} executable", script.display()))?;
    }
    std::fs::copy(artifacts.rootfs_file.get(), &rootfs_path).with_context(|| {
        format!(
            "failed to copy CCA rootfs from {} to {}",
            artifacts.rootfs_file.get().display(),
            rootfs_path.display()
        )
    })?;
    tracing::info!(
        source = %artifacts.rootfs_file.get().display(),
        rootfs = %rootfs_path.display(),
        "copied CCA rootfs for test run"
    );

    fsck_rootfs(artifacts.e2fsck_bin.get(), &rootfs_path)?;
    tracing::info!("e2fsck finished");

    resize_rootfs(artifacts.resize2fs_bin.get(), &rootfs_path, "1024M")?;
    tracing::info!("resize rootfs to 1024M finished");

    let cca_files = [
        (artifacts.openvmm_bin.get(), Path::new("cca/openvmm")),
        (artifacts.tmk_vmm_bin.get(), Path::new("cca/tmk_vmm")),
        (artifacts.guest_disk.get(), Path::new("cca/guest-disk.img")),
        (artifacts.plane0_linux_image.get(), Path::new("cca/Image")),
        (artifacts.kvmtool_efi.get(), Path::new("cca/KVMTOOL_EFI.fd")),
        (artifacts.lkvm.get(), Path::new("cca/lkvm")),
        (artifacts.uefi_igvm.get(), Path::new("cca/uefi-aarch64.bin")),
        (start_tmk_path.as_path(), Path::new("root/start-tmk.sh")),
        (
            run_realm_test_path.as_path(),
            Path::new("usr/local/bin/run_realm_test.sh"),
        ),
    ];
    inject_files_into_cca_rootfs(&rootfs_path, &cca_files)?;

    tracing::info!(
        "rootfs.ext2 updated successfully with cca firmwares, paravisor, and tests injected"
    );

    Ok(PreparedCcaRootfs {
        test_dir,
        rootfs_path,
    })
}

fn fsck_rootfs(e2fsck_bin: &Path, rootfs_file: &Path) -> anyhow::Result<()> {
    let status = Command::new(e2fsck_bin)
        .arg("-fp")
        .arg(rootfs_file)
        .status()
        .with_context(|| format!("failed to execute {}", e2fsck_bin.display()))?;

    // e2fsck returns 1 when filesystem errors were found and corrected,
    // which is common after killing the FVP and leaving the rootfs dirty.
    match status.code() {
        Some(0 | 1) => Ok(()),
        Some(code) => anyhow::bail!("e2fsck failed with exit code {code}"),
        None => anyhow::bail!("e2fsck was terminated by signal"),
    }
}

fn resize_rootfs(resize2fs_bin: &Path, rootfs_file: &Path, size: &str) -> anyhow::Result<()> {
    let status = Command::new(resize2fs_bin)
        .arg(rootfs_file)
        .arg(size)
        .status()
        .with_context(|| format!("failed to execute {}", resize2fs_bin.display()))?;

    if !status.success() {
        anyhow::bail!("resize2fs failed with exit status {status}");
    }

    Ok(())
}

fn run_sudo(description: &str, args: &[&OsStr]) -> anyhow::Result<()> {
    let status = Command::new("sudo")
        .arg("-n")
        .args(args)
        .status()
        .with_context(|| format!("failed to execute sudo command to {description}"))?;

    if !status.success() {
        anyhow::bail!(
            "failed to {description}: exit status {status}; CCA tests require non-interactive sudo for rootfs mount/injection commands. Configure passwordless sudo for the required commands or run in an environment where sudo -n is allowed."
        );
    }

    Ok(())
}

fn inject_files_into_cca_rootfs(
    rootfs_file: &Path,
    files: &[(&Path, &Path)],
) -> anyhow::Result<()> {
    let mount_dir = tempfile::tempdir().context("failed to create guest rootfs mount directory")?;
    let mnt_dir = mount_dir.path().to_path_buf();

    let mut mounted = false;
    let inject_result = (|| -> anyhow::Result<()> {
        run_sudo(
            "mount guest rootfs",
            &[
                OsStr::new("mount"),
                OsStr::new("-o"),
                OsStr::new("loop"),
                rootfs_file.as_os_str(),
                mnt_dir.as_os_str(),
            ],
        )?;
        mounted = true;

        for (file, target) in files {
            let target_file = mnt_dir.join(target);
            let target_dir = target_file
                .parent()
                .context("CCA rootfs target has no parent directory")?;
            run_sudo(
                &format!("create {} in guest rootfs", target_dir.display()),
                &[
                    OsStr::new("mkdir"),
                    OsStr::new("-p"),
                    target_dir.as_os_str(),
                ],
            )?;
            run_sudo(
                &format!(
                    "copy {} into guest rootfs as {}",
                    file.display(),
                    target_file.display()
                ),
                &[OsStr::new("cp"), file.as_os_str(), target_file.as_os_str()],
            )?;
        }

        let initramfs_dir =
            tempfile::tempdir().context("failed to create VTL0 initramfs build directory")?;
        let initramfs = initramfs_dir.path().join("vtl0-initramfs.cpio");
        build_vtl0_initramfs(&initramfs, &mnt_dir)?;
        let initramfs_target = mnt_dir.join(CCA_VTL0_INITRAMFS_PATH);
        run_sudo(
            "copy VTL0 initramfs into guest rootfs",
            &[
                OsStr::new("cp"),
                initramfs.as_os_str(),
                initramfs_target.as_os_str(),
            ],
        )?;

        run_sudo("sync guest rootfs writes", &[OsStr::new("sync")])?;

        Ok(())
    })();

    if mounted {
        if let Err(err) = run_sudo(
            "unmount guest rootfs",
            &[OsStr::new("umount"), mnt_dir.as_os_str()],
        )
        .or_else(|_| {
            run_sudo(
                "lazy unmount guest rootfs",
                &[OsStr::new("umount"), OsStr::new("-l"), mnt_dir.as_os_str()],
            )
        }) {
            tracing::warn!(error = err.as_ref() as &dyn std::error::Error, "{err:#}");
        }
    }

    if let Err(err) = run_sudo("sync host writes", &[OsStr::new("sync")]) {
        tracing::warn!(error = err.as_ref() as &dyn std::error::Error, "{err:#}");
    }

    thread::sleep(Duration::from_secs(1));
    for _ in 0..5 {
        if !mnt_dir.is_dir() {
            break;
        }

        if run_sudo(
            "remove guest rootfs mount directory",
            &[OsStr::new("rmdir"), mnt_dir.as_os_str()],
        )
        .is_ok()
        {
            break;
        }

        thread::sleep(Duration::from_millis(500));
    }

    if mnt_dir.is_dir() {
        if let Err(err) = run_sudo(
            "force remove guest rootfs mount directory",
            &[OsStr::new("rm"), OsStr::new("-rf"), mnt_dir.as_os_str()],
        ) {
            tracing::warn!(error = err.as_ref() as &dyn std::error::Error, "{err:#}");
        }
    }

    inject_result.with_context(|| "failed to mount or inject files into guest rootfs")
}

fn build_vtl0_initramfs(output: &Path, rootfs: &Path) -> anyhow::Result<()> {
    let stage = tempfile::tempdir().context("failed to create VTL0 initramfs staging directory")?;
    let stage = stage.path();

    for directory in ["bin", "dev", "lib", "proc", "root", "sys", "tmp"] {
        std::fs::create_dir_all(stage.join(directory))
            .with_context(|| format!("failed to create initramfs /{directory}"))?;
    }
    symlink("lib", stage.join("lib64"))
        .context("failed to create the VTL0 initramfs /lib64 compatibility symlink")?;

    let init_source = stage.join("init.c");
    let init = stage.join("init");
    let init_script = stage.join("init.sh");
    let staged_busybox = stage.join("bin/busybox");
    std::fs::write(&init_source, CCA_VTL0_INIT_SOURCE)
        .context("failed to write VTL0 init source")?;
    std::fs::write(&init_script, CCA_VTL0_INIT_SCRIPT)
        .context("failed to write VTL0 init script")?;
    std::fs::set_permissions(&init_script, std::fs::Permissions::from_mode(0o755))
        .context("failed to make VTL0 init script executable")?;
    let busybox = rootfs.join("bin/busybox");
    std::fs::copy(&busybox, &staged_busybox).with_context(|| {
        format!(
            "failed to copy BusyBox from {} into the VTL0 initramfs",
            busybox.display()
        )
    })?;
    std::fs::set_permissions(&staged_busybox, std::fs::Permissions::from_mode(0o755))
        .context("failed to make VTL0 BusyBox executable")?;
    for library in [
        "lib/ld-linux-aarch64.so.1",
        "lib/libc.so.6",
        "lib/libresolv.so.2",
    ] {
        let source = rootfs.join(library);
        let target = stage.join(library);
        std::fs::copy(&source, &target).with_context(|| {
            format!(
                "failed to copy BusyBox runtime dependency {} into the VTL0 initramfs",
                source.display()
            )
        })?;
    }
    let compile = Command::new("aarch64-linux-gnu-gcc")
        .args([
            "-nostdlib",
            "-static",
            "-no-pie",
            "-Os",
            "-fno-stack-protector",
            "-fno-asynchronous-unwind-tables",
            "-Wl,--build-id=none",
            "-Wl,-e,_start",
            "-o",
        ])
        .arg(&init)
        .arg(&init_source)
        .output()
        .context("failed to launch aarch64-linux-gnu-gcc for VTL0 init")?;
    if !compile.status.success() {
        anyhow::bail!(
            "failed to compile VTL0 init: {}",
            String::from_utf8_lossy(&compile.stderr).trim()
        );
    }
    std::fs::remove_file(&init_source).context("failed to remove staged VTL0 init source")?;

    for (name, major, minor) in [("console", "5", "1"), ("null", "1", "3")] {
        let device = stage.join("dev").join(name);
        run_sudo(
            &format!("create VTL0 initramfs /dev/{name}"),
            &[
                OsStr::new("mknod"),
                device.as_os_str(),
                OsStr::new("c"),
                OsStr::new(major),
                OsStr::new(minor),
            ],
        )?;
    }

    let archive = File::create(output)
        .with_context(|| format!("failed to create VTL0 initramfs {}", output.display()))?;
    let mut cpio = Command::new("bsdcpio")
        .args(["-o", "--format", "newc", "--owner", "0:0"])
        .current_dir(stage)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(archive))
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch bsdcpio for VTL0 initramfs")?;

    {
        let stdin = cpio
            .stdin
            .as_mut()
            .context("failed to capture bsdcpio stdin")?;
        for entry in walkdir::WalkDir::new(stage).sort_by_file_name() {
            let entry = entry.context("failed to enumerate VTL0 initramfs")?;
            let relative = entry
                .path()
                .strip_prefix(stage)
                .context("initramfs entry was outside its staging directory")?;
            if relative.as_os_str().is_empty() {
                writeln!(stdin, ".")?;
            } else {
                writeln!(stdin, "./{}", relative.display())?;
            }
        }
    }

    let result = cpio
        .wait_with_output()
        .context("failed to wait for VTL0 initramfs bsdcpio")?;
    if !result.status.success() {
        anyhow::bail!(
            "bsdcpio failed to build VTL0 initramfs: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }

    tracing::info!(
        path = %output.display(),
        size = output.metadata()?.len(),
        "built IRQ-sensitive VTL0 Linux initramfs"
    );
    Ok(())
}

fn run_shrinkwrap_cca_test(
    shrinkwrap_exe: &Path,
    venv_dir: &Path,
    rootfs_file: &Path,
    venv_bin_path: &str,
    stdout_log: petri::PetriLogFile,
    stderr_log: petri::PetriLogFile,
) -> anyhow::Result<()> {
    let pause_before_start_tmk = pause_before_start_tmk();
    let interactive_vtl0_shell = interactive_vtl0_shell();
    if pause_before_start_tmk && interactive_vtl0_shell {
        anyhow::bail!(
            "{CCA_PAUSE_BEFORE_START_TMK_ENV} and {CCA_INTERACTIVE_VTL0_SHELL_ENV} cannot both be enabled"
        );
    }

    let mut emu = start_cca_emulator(
        shrinkwrap_exe,
        venv_dir,
        rootfs_file,
        venv_bin_path,
        stdout_log,
        stderr_log,
        !interactive_vtl0_shell,
    )?;

    emu.wait_for(CCA_PLANE0_PROMPT)?;
    if pause_before_start_tmk {
        emu.interactive_pause_before_start_tmk(rootfs_file)?;
    }
    emu.send_line(CCA_START_TMK_COMMAND)?;
    emu.wait_for(CCA_VTL0_TIMER_IRQ_MARKER)?;
    emu.wait_for(CCA_VTL0_SHELL_READY_MARKER)?;
    if interactive_vtl0_shell {
        emu.interactive_vtl0_console(rootfs_file)?;
        emu.synchronize_vtl0_shell()?;
    }
    emu.send_line(CCA_VTL0_SHELL_COMMAND)?;
    emu.wait_for(CCA_TEST_SUCCESS_MARKER)?;
    // Need to manually kill FVP processes for the petri test since shrinkwrap doesn't wait
    // for them to exit and they can interfere with subsequent test runs if left running
    stop_fvp_processes_for_rootfs(rootfs_file)?;

    let status = emu
        .stop()
        .context("failed to stop shrinkwrap after CCA test completed")?;
    tracing::info!("CCA test passed; stopped shrinkwrap process with status {status}");

    Ok(())
}

fn pause_before_start_tmk() -> bool {
    environment_flag_is_set(CCA_PAUSE_BEFORE_START_TMK_ENV)
}

fn interactive_vtl0_shell() -> bool {
    environment_flag_is_set(CCA_INTERACTIVE_VTL0_SHELL_ENV)
}

fn environment_flag_is_set(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}

fn stop_fvp_processes_for_rootfs(rootfs_file: &Path) -> anyhow::Result<()> {
    let output = Command::new("pgrep")
        .arg("-f")
        .arg(rootfs_file)
        .output()
        .context("failed to execute pgrep for CCA FVP process")?;

    match output.status.code() {
        Some(0) => {}
        Some(1) => {
            tracing::warn!(rootfs = %rootfs_file.display(), "found no FVP process for CCA test rootfs");
            return Ok(());
        }
        Some(code) => anyhow::bail!("pgrep for CCA FVP process failed with exit code {code}"),
        None => anyhow::bail!("pgrep for CCA FVP process was terminated by signal"),
    }

    let stdout = String::from_utf8(output.stdout).context("pgrep output was not utf-8")?;
    for line in stdout.lines() {
        let pid = line
            .parse::<u32>()
            .with_context(|| format!("failed to parse pgrep pid `{line}`"))?;
        tracing::info!(pid, rootfs = %rootfs_file.display(), "found CCA FVP process for test rootfs");
        terminate_process(pid, "TERM")?;
    }

    Ok(())
}

fn terminate_process(pid: u32, signal: &str) -> anyhow::Result<()> {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .with_context(|| format!("failed to execute kill -{signal} {pid}"))?;

    match status.code() {
        Some(0) => {
            tracing::info!(pid, signal, "sent signal to CCA FVP process");
            Ok(())
        }
        Some(1) => {
            tracing::warn!(pid, signal, "CCA FVP process was already gone");
            Ok(())
        }
        Some(code) => anyhow::bail!("kill -{signal} {pid} failed with exit code {code}"),
        None => anyhow::bail!("kill -{signal} {pid} was terminated by signal"),
    }
}

fn start_cca_emulator(
    shrinkwrap_exe: &Path,
    venv_dir: &Path,
    rootfs_file: &Path,
    venv_bin_path: &str,
    stdout_log: petri::PetriLogFile,
    stderr_log: petri::PetriLogFile,
    emit_log_stdout: bool,
) -> anyhow::Result<CcaEmulator> {
    let child = Command::new(shrinkwrap_exe)
        .args(["run", "cca-3world.yaml", "--rtvar"])
        .arg(format!("ROOTFS={}", rootfs_file.display()))
        .env("VIRTUAL_ENV", venv_dir)
        .env("PATH", venv_bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "failed to launch guest using shrinkwrap")?;
    let mut child = ChildGuard::new(child);

    let stdin = child
        .as_mut()
        .stdin
        .take()
        .context("failed to capture shrinkwrap stdin")?;
    let stdout = child
        .as_mut()
        .stdout
        .take()
        .context("failed to capture shrinkwrap stdout")?;
    let stderr = child
        .as_mut()
        .stderr
        .take()
        .context("failed to capture shrinkwrap stderr")?;
    let (output_send, output_recv) = mpsc::channel::<String>();

    spawn_output_reader(
        "shrinkwrap stdout",
        stdout,
        stdout_log,
        output_send.clone(),
        emit_log_stdout,
    );
    spawn_output_reader(
        "shrinkwrap stderr",
        stderr,
        stderr_log,
        output_send.clone(),
        emit_log_stdout,
    );
    drop(output_send);

    Ok(CcaEmulator {
        child,
        stdin,
        output_recv,
        output: String::new(),
        started: Instant::now(),
    })
}

struct ChildGuard {
    child: Option<std::process::Child>,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self { child: Some(child) }
    }

    fn as_mut(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("shrinkwrap child is missing")
    }

    fn kill_and_wait(&mut self) -> anyhow::Result<std::process::ExitStatus> {
        let Some(mut child) = self.child.take() else {
            anyhow::bail!("shrinkwrap child is missing");
        };

        let _ = child.kill();
        child
            .wait()
            .context("failed to wait for shrinkwrap process")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct CcaEmulator {
    child: ChildGuard,
    stdin: std::process::ChildStdin,
    output_recv: mpsc::Receiver<String>,
    output: String,
    started: Instant,
}

impl CcaEmulator {
    fn wait_for(&mut self, marker: &str) -> anyhow::Result<()> {
        tracing::info!(marker, "waiting for CCA emulator output");

        loop {
            if self.output.contains(marker) {
                tracing::info!(marker, "observed CCA emulator output");
                return Ok(());
            }

            if let Some(failure_marker) = CCA_TEST_FAILURE_MARKERS
                .iter()
                .find(|marker| self.output.contains(**marker))
            {
                anyhow::bail!(
                    "CCA test failed after observing failure marker `{failure_marker}` while waiting for `{marker}`"
                );
            }

            if let Some(status) = self.child.as_mut().try_wait()? {
                anyhow::bail!(
                    "shrinkwrap exited before CCA emulator output `{marker}` was observed: {status}"
                );
            }

            let remaining = CCA_TEST_TIMEOUT
                .checked_sub(self.started.elapsed())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                let _ = self.child.kill_and_wait();
                anyhow::bail!("timed out waiting for CCA emulator output `{marker}`");
            }

            match self
                .output_recv
                .recv_timeout(remaining.min(Duration::from_millis(500)))
            {
                Ok(output) => self.append_output(&output),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let status = if let Some(status) = self.child.as_mut().try_wait()? {
                        status
                    } else {
                        self.child.kill_and_wait().with_context(|| {
                            format!(
                                "failed to stop shrinkwrap after output ended while waiting for `{marker}`"
                            )
                        })?
                    };
                    anyhow::bail!(
                        "shrinkwrap output ended before CCA emulator output `{marker}` was observed: {status}"
                    );
                }
            }
        }
    }

    fn append_output(&mut self, output: &str) {
        self.output.push_str(output);

        if self.output.len() <= CCA_OUTPUT_WINDOW_SIZE {
            return;
        }

        let drain_len = self.output.len() - CCA_OUTPUT_WINDOW_SIZE;
        let drain_to = self
            .output
            .char_indices()
            .map(|(idx, _)| idx)
            .find(|idx| *idx >= drain_len)
            .unwrap_or(self.output.len());
        self.output.drain(..drain_to);
    }

    fn send_line(&mut self, line: &str) -> anyhow::Result<()> {
        tracing::info!(line, "sending CCA emulator command");
        writeln!(self.stdin, "{line}").context("failed to write command to shrinkwrap stdin")?;
        self.stdin
            .flush()
            .context("failed to flush command to shrinkwrap stdin")
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.stdin
            .write_all(bytes)
            .context("failed to write terminal input to shrinkwrap stdin")?;
        self.stdin
            .flush()
            .context("failed to flush terminal input to shrinkwrap stdin")
    }

    fn synchronize_vtl0_shell(&mut self) -> anyhow::Result<()> {
        while let Ok(output) = self.output_recv.try_recv() {
            self.append_output(&output);
        }
        self.output.clear();
        self.send_line("")?;
        self.wait_for(CCA_VTL0_SHELL_PROMPT)
    }

    fn interactive_pause_before_start_tmk(&mut self, rootfs_file: &Path) -> anyhow::Result<()> {
        self.interactive_pause(rootfs_file, "before starting TMK")
    }

    fn interactive_vtl0_console(&mut self, rootfs_file: &Path) -> anyhow::Result<()> {
        tracing::warn!(
            rootfs = %rootfs_file.display(),
            "CCA test entering the interactive VTL0 Linux console"
        );

        let mut terminal_output = open_terminal_output();
        self.send_line("")?;
        writeln!(
            terminal_output,
            "\nVTL0 Linux console. Press Ctrl+] to finish the interactive session."
        )
        .context("failed to write VTL0 console prompt")?;
        terminal_output
            .flush()
            .context("failed to flush VTL0 console prompt")?;

        let mut input = File::open("/dev/tty")
            .context("the interactive VTL0 console requires a controlling terminal")?;
        let _raw_mode = RawModeGuard::enter(&input)?;
        let (input_send, input_recv) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0; 256];
            loop {
                let event = match input.read(&mut buffer) {
                    Ok(0) => TerminalInputEvent::Eof,
                    Ok(len) => TerminalInputEvent::Bytes(buffer[..len].to_vec()),
                    Err(err) => TerminalInputEvent::Error(err.to_string()),
                };
                let done = matches!(
                    event,
                    TerminalInputEvent::Eof | TerminalInputEvent::Error(_)
                );
                if input_send.send(event).is_err() || done {
                    return;
                }
            }
        });

        loop {
            match input_recv.try_recv() {
                Ok(TerminalInputEvent::Bytes(bytes)) => {
                    if let Some(escape) = bytes
                        .iter()
                        .position(|&byte| byte == CCA_INTERACTIVE_ESCAPE)
                    {
                        self.send_bytes(&bytes[..escape])?;
                        terminal_output.write_all(b"\r\n")?;
                        terminal_output.flush()?;
                        self.started = Instant::now();
                        return Ok(());
                    }
                    self.send_bytes(&bytes)?;
                }
                Ok(TerminalInputEvent::Eof) => {
                    anyhow::bail!("terminal input closed during the interactive VTL0 console");
                }
                Ok(TerminalInputEvent::Error(err)) => {
                    anyhow::bail!("failed to read interactive terminal input: {err}");
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    anyhow::bail!("terminal input ended during the interactive VTL0 console");
                }
            }

            if let Some(status) = self.child.as_mut().try_wait()? {
                anyhow::bail!("shrinkwrap exited during the interactive VTL0 console: {status}");
            }

            match self.output_recv.recv_timeout(Duration::from_millis(20)) {
                Ok(output) => {
                    self.append_output(&output);
                    terminal_output
                        .write_all(output.as_bytes())
                        .context("failed to write VTL0 console output to the terminal")?;
                    terminal_output
                        .flush()
                        .context("failed to flush VTL0 console output")?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let status = if let Some(status) = self.child.as_mut().try_wait()? {
                        status
                    } else {
                        self.child.kill_and_wait().with_context(
                            || "failed to stop shrinkwrap after VTL0 console output ended",
                        )?
                    };
                    anyhow::bail!("VTL0 console output ended before the session exited: {status}");
                }
            }
        }
    }

    fn interactive_pause(&mut self, rootfs_file: &Path, location: &str) -> anyhow::Result<()> {
        tracing::warn!(
            rootfs = %rootfs_file.display(),
            continue_command = CCA_PAUSE_CONTINUE_COMMAND,
            location,
            "CCA test paused for interactive input"
        );

        let mut terminal_output = open_terminal_output();
        writeln!(
            terminal_output,
            "\nCCA test paused {location}.\n\
             Rootfs: {}\n\
             Type guest commands here. Enter `{}` on its own line to continue the test.",
            rootfs_file.display(),
            CCA_PAUSE_CONTINUE_COMMAND
        )
        .context("failed to write CCA pause prompt")?;
        terminal_output
            .flush()
            .context("failed to flush CCA pause prompt")?;

        let input = open_terminal_input().context("failed to open terminal input for CCA pause")?;
        let (input_send, input_recv) = mpsc::channel();
        thread::spawn(move || {
            for line in input.lines() {
                if input_send.send(line).is_err() {
                    return;
                }
            }
        });

        loop {
            match input_recv.try_recv() {
                Ok(line) => {
                    let line = line.context("failed to read CCA pause command")?;
                    if line.trim() == CCA_PAUSE_CONTINUE_COMMAND {
                        self.started = Instant::now();
                        tracing::info!("resuming CCA test after interactive pause");
                        return Ok(());
                    }

                    self.send_line(&line)?;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    anyhow::bail!("terminal input closed during CCA interactive pause");
                }
            }

            if let Some(status) = self.child.as_mut().try_wait()? {
                anyhow::bail!("shrinkwrap exited while CCA test was paused: {status}");
            }

            match self.output_recv.recv_timeout(Duration::from_millis(100)) {
                Ok(output) => {
                    self.append_output(&output);
                    terminal_output
                        .write_all(output.as_bytes())
                        .context("failed to write CCA emulator output to terminal")?;
                    terminal_output
                        .flush()
                        .context("failed to flush CCA emulator output to terminal")?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let status = if let Some(status) = self.child.as_mut().try_wait()? {
                        status
                    } else {
                        self.child.kill_and_wait().with_context(
                            || "failed to stop shrinkwrap after output ended during CCA pause",
                        )?
                    };
                    anyhow::bail!("shrinkwrap output ended while CCA test was paused: {status}");
                }
            }
        }
    }

    fn stop(mut self) -> anyhow::Result<std::process::ExitStatus> {
        self.child.kill_and_wait()
    }
}

fn open_terminal_input() -> std::io::Result<Box<dyn BufRead + Send>> {
    File::open("/dev/tty")
        .map(|tty| Box::new(BufReader::new(tty)) as Box<dyn BufRead + Send>)
        .or_else(|_| Ok(Box::new(BufReader::new(std::io::stdin())) as Box<dyn BufRead + Send>))
}

fn open_terminal_output() -> Box<dyn Write> {
    OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .map(|tty| Box::new(tty) as Box<dyn Write>)
        .unwrap_or_else(|_| Box::new(std::io::stderr()) as Box<dyn Write>)
}

struct RawModeGuard {
    terminal: File,
    original: termios::Termios,
}

impl RawModeGuard {
    fn enter(terminal: &File) -> anyhow::Result<Self> {
        let terminal = terminal
            .try_clone()
            .context("failed to clone terminal handle")?;
        let original =
            termios::tcgetattr(&terminal).context("failed to read terminal attributes")?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        raw.output_flags = original.output_flags;
        termios::tcsetattr(&terminal, termios::SetArg::TCSANOW, &raw)
            .context("failed to enable terminal raw mode")?;
        Ok(Self { terminal, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Err(err) =
            termios::tcsetattr(&self.terminal, termios::SetArg::TCSANOW, &self.original)
        {
            tracing::warn!(error = %err, "failed to restore terminal mode");
        }
    }
}

enum TerminalInputEvent {
    Bytes(Vec<u8>),
    Eof,
    Error(String),
}

enum OutputEvent {
    Byte(u8),
    Eof,
    Error(String),
}

fn spawn_output_reader(
    stream_name: &'static str,
    mut stream: impl Read + Send + 'static,
    log_file: petri::PetriLogFile,
    output_send: mpsc::Sender<String>,
    emit_log_stdout: bool,
) {
    let (byte_send, byte_recv) = mpsc::channel();

    thread::spawn(move || {
        let mut byte = [0];

        loop {
            let event = match stream.read(&mut byte) {
                Ok(0) => OutputEvent::Eof,
                Ok(_) => OutputEvent::Byte(byte[0]),
                Err(err) => OutputEvent::Error(format!("failed to read {stream_name}: {err}")),
            };

            let done = matches!(event, OutputEvent::Eof | OutputEvent::Error(_));
            if byte_send.send(event).is_err() || done {
                break;
            }
        }
    });

    thread::spawn(move || {
        let mut line = Vec::new();
        let mut logged_len = 0;

        loop {
            match byte_recv.recv_timeout(Duration::from_millis(100)) {
                Ok(OutputEvent::Byte(b'\n')) => {
                    if line.ends_with(b"\r") {
                        line.pop();
                    }
                    let line_was_empty = line.is_empty();
                    write_output_line(
                        &log_file,
                        &output_send,
                        &mut line,
                        &mut logged_len,
                        emit_log_stdout,
                    );
                    write_output_newline(&log_file, &output_send, line_was_empty, emit_log_stdout);
                }
                Ok(OutputEvent::Byte(byte)) => {
                    line.push(byte);
                }
                Ok(OutputEvent::Eof) => {
                    write_output_line(
                        &log_file,
                        &output_send,
                        &mut line,
                        &mut logged_len,
                        emit_log_stdout,
                    );
                    break;
                }
                Ok(OutputEvent::Error(line)) => {
                    write_log_entry(&log_file, &line, emit_log_stdout);
                    let _ = output_send.send(line);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    write_partial_output(&line, &mut logged_len, &output_send);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    write_output_line(
                        &log_file,
                        &output_send,
                        &mut line,
                        &mut logged_len,
                        emit_log_stdout,
                    );
                    break;
                }
            }
        }
    });
}

fn write_output_line(
    log_file: &petri::PetriLogFile,
    output_send: &mpsc::Sender<String>,
    line: &mut Vec<u8>,
    logged_len: &mut usize,
    emit_log_stdout: bool,
) {
    if line.is_empty() {
        return;
    }

    write_visible_output(line, logged_len, output_send);

    let line_string = String::from_utf8_lossy(line).into_owned();
    write_log_entry(log_file, &line_string, emit_log_stdout);
    line.clear();
    *logged_len = 0;
}

fn write_output_newline(
    log_file: &petri::PetriLogFile,
    output_send: &mpsc::Sender<String>,
    line_was_empty: bool,
    emit_log_stdout: bool,
) {
    let _ = output_send.send("\n".to_owned());

    if line_was_empty {
        write_log_entry(log_file, "", emit_log_stdout);
    }
}

fn write_log_entry(log_file: &petri::PetriLogFile, line: &str, emit_stdout: bool) {
    if emit_stdout {
        log_file.write_entry(line);
    } else {
        log_file.write_entry_silent(line);
    }
}

fn write_visible_output(line: &[u8], logged_len: &mut usize, output_send: &mpsc::Sender<String>) {
    if *logged_len >= line.len() {
        return;
    }

    let output = &line[*logged_len..];
    let _ = output_send.send(String::from_utf8_lossy(output).into_owned());
    *logged_len = line.len();
}

fn write_partial_output(line: &[u8], logged_len: &mut usize, output_send: &mpsc::Sender<String>) {
    let visible = line.strip_suffix(b"\r").unwrap_or(line);
    write_visible_output(visible, logged_len, output_send);
}

petri::multitest!(vec![
    petri::SimpleTest::new(
        "cca_runtime",
        resolve_cca_runtime,
        cca_runtime,
        None,
        false,
        petri::RemoteAccess::LocalOnly,
    )
    .into()
]);

fn main() {
    petri::test_main(|name, requirements| {
        requirements.resolve(
            petri_artifact_resolver_openvmm_known_paths::OpenvmmKnownPathsTestArtifactResolver::new(
                name,
            ),
        )
    })
}

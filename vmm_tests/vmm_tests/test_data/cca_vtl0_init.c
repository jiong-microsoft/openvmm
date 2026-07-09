// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// A dependency-free AArch64 PID 1 for the CCA timer IRQ test.

typedef unsigned long usize;

struct timespec {
    long tv_sec;
    long tv_nsec;
};

static long syscall2(long number, long arg0, long arg1)
{
    register long x0 __asm__("x0") = arg0;
    register long x1 __asm__("x1") = arg1;
    register long x8 __asm__("x8") = number;

    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x8) : "memory");
    return x0;
}

static long syscall3(long number, long arg0, long arg1, long arg2)
{
    register long x0 __asm__("x0") = arg0;
    register long x1 __asm__("x1") = arg1;
    register long x2 __asm__("x2") = arg2;
    register long x8 __asm__("x8") = number;

    __asm__ volatile(
        "svc 0"
        : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x8)
        : "memory"
    );
    return x0;
}

static long syscall4(long number, long arg0, long arg1, long arg2, long arg3)
{
    register long x0 __asm__("x0") = arg0;
    register long x1 __asm__("x1") = arg1;
    register long x2 __asm__("x2") = arg2;
    register long x3 __asm__("x3") = arg3;
    register long x8 __asm__("x8") = number;

    __asm__ volatile(
        "svc 0"
        : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x3), "r"(x8)
        : "memory"
    );
    return x0;
}

static long syscall5(
    long number,
    long arg0,
    long arg1,
    long arg2,
    long arg3,
    long arg4
)
{
    register long x0 __asm__("x0") = arg0;
    register long x1 __asm__("x1") = arg1;
    register long x2 __asm__("x2") = arg2;
    register long x3 __asm__("x3") = arg3;
    register long x4 __asm__("x4") = arg4;
    register long x8 __asm__("x8") = number;

    __asm__ volatile(
        "svc 0"
        : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x8)
        : "memory"
    );
    return x0;
}

static void write_message(const char *message, usize length)
{
    const long sys_write = 64;
    const long stdout_fd = 1;

    (void)syscall3(sys_write, stdout_fd, (long)message, (long)length);
}

__attribute__((noreturn)) static void exit_process(long status)
{
    const long sys_exit = 93;
    register long x0 __asm__("x0") = status;
    register long x8 __asm__("x8") = sys_exit;

    __asm__ volatile("svc 0" : : "r"(x0), "r"(x8) : "memory");
    __builtin_unreachable();
}

__attribute__((noreturn)) void _start(void)
{
    static const char booted[] = "CCA_VTL0_LINUX_BOOTED\n";
    static const char waiting[] = "CCA_VTL0_WAITING_FOR_TIMER_IRQ\n";
    static const char success[] = "CCA_VTL0_TIMER_IRQ_OK\n";
    static const char failed[] = "CCA_VTL0_TIMER_SLEEP_FAILED\n";
    static const char console_failed[] = "CCA_VTL0_CONSOLE_SETUP_FAILED\n";
    static const char shell_failed[] = "CCA_VTL0_SHELL_EXEC_FAILED\n";
    static const char console[] = "/dev/console";
    static const char busybox[] = "/bin/busybox";
    static const char shell[] = "sh";
    static const char init_script[] = "/init.sh";
    static const char path[] = "PATH=/bin";
    const long sys_nanosleep = 101;
    const long sys_clone = 220;
    const long sys_setsid = 157;
    const long sys_openat = 56;
    const long sys_ioctl = 29;
    const long sys_dup3 = 24;
    const long sys_close = 57;
    const long sys_getpid = 172;
    const long sys_execve = 221;
    const long sys_wait4 = 260;
    const long interrupted = -4;
    struct timespec remaining = { .tv_sec = 0, .tv_nsec = 10 * 1000 * 1000 };
    long status;

    write_message(booted, sizeof(booted) - 1);
    write_message(waiting, sizeof(waiting) - 1);

    do {
        struct timespec requested = remaining;
        status = syscall2(sys_nanosleep, (long)&requested, (long)&remaining);
    } while (status == interrupted);

    if (status == 0) {
        const long signal_child = 17;
        long child;

        write_message(success, sizeof(success) - 1);
        child = syscall5(sys_clone, signal_child, 0, 0, 0, 0);
        if (child == 0) {
            const long at_fdcwd = -100;
            const long open_read_write = 2;
            const long tiocsctty = 0x540e;
            const long tiocspgrp = 0x5410;
            const char *argv[] = { busybox, shell, init_script, 0 };
            const char *envp[] = { path, 0 };
            long console_fd;
            long foreground_process_group;

            if (syscall2(sys_setsid, 0, 0) < 0) {
                write_message(console_failed, sizeof(console_failed) - 1);
                exit_process(126);
            }

            console_fd = syscall4(
                sys_openat,
                at_fdcwd,
                (long)console,
                open_read_write,
                0
            );
            foreground_process_group = syscall2(sys_getpid, 0, 0);
            if (console_fd < 0
                || syscall3(sys_ioctl, console_fd, tiocsctty, 0) < 0
                || syscall3(
                       sys_ioctl,
                       console_fd,
                       tiocspgrp,
                       (long)&foreground_process_group
                   ) < 0
                || syscall3(sys_dup3, console_fd, 0, 0) < 0
                || syscall3(sys_dup3, console_fd, 1, 0) < 0
                || syscall3(sys_dup3, console_fd, 2, 0) < 0)
            {
                write_message(console_failed, sizeof(console_failed) - 1);
                exit_process(126);
            }

            if (console_fd > 2) {
                (void)syscall2(sys_close, console_fd, 0);
            }

            (void)syscall3(sys_execve, (long)busybox, (long)argv, (long)envp);
            write_message(shell_failed, sizeof(shell_failed) - 1);
            exit_process(127);
        } else if (child > 0) {
            (void)syscall4(sys_wait4, child, (long)&status, 0, 0);
            write_message(shell_failed, sizeof(shell_failed) - 1);
            exit_process(127);
        } else {
            write_message(shell_failed, sizeof(shell_failed) - 1);
            exit_process(127);
        }
    } else {
        write_message(failed, sizeof(failed) - 1);
        exit_process(1);
    }
}

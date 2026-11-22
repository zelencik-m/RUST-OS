#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use rust_os::fs::devices::{stderr_file, stdin_file, stdout_file};
use rust_os::fs::Path;
use rust_os::process::process_manager::{self, SpawnArgs, PROCESS_MANAGER};
use rust_os::process::{CHILD_EXITS, PID};
use rust_os::programs;
use rust_os::scheduler::scheduler::sleep;
use rust_os::scheduler::wait_queue::WaitQueue;
use rust_os::debug::Level;
use rust_os::{boot_screen, hlt_loop, log_info, println, serial_println, status_bar, theme, tty};
use x86_64::instructions::interrupts;

entry_point!(kernel_main);

#[repr(C, align(8))]
struct AlignedBytes<const N: usize>([u8; N]);

macro_rules! embedded_elf {
    ($name:ident, $file:literal) => {
        static $name: AlignedBytes<{ include_bytes!(concat!(env!("OUT_DIR"), "/", $file)).len() }> =
            AlignedBytes(*include_bytes!(concat!(env!("OUT_DIR"), "/", $file)));
    };
}

embedded_elf!(SH, "sh.elf");
embedded_elf!(HELLO, "hello.elf");
embedded_elf!(THREAD_B, "thread_b.elf");
embedded_elf!(THREAD_C, "thread_c.elf");

/// Installed to `/bin/<name>` at every boot (see `rust_os::programs`).
fn installed_programs() -> [(&'static str, &'static [u8]); 4] {
    [
        ("sh", &SH.0),
        ("hello", &HELLO.0),
        ("thread_b", &THREAD_B.0),
        ("thread_c", &THREAD_C.0),
    ]
}

/// The first user process, started from the disk.
const SHELL_PATH: &str = "/bin/sh";

/// Ticks to wait before starting a new shell, so a shell that keeps dying
/// right away doesn't take over the CPU.
const RESTART_DELAY_TICKS: u64 = 9;

/// The kernel log level for a normal boot (serial only; see
/// `rust_os::debug`). `Info` gives the boot steps, the shell starting, and
/// warnings such as a program being killed. `Debug` adds per-subsystem
/// details; `Trace` logs every context switch, which floods the serial copy
/// of the shell's output. `None` turns logging off.
const LOG_LEVEL: Option<Level> = Some(Level::Info);

/// The boot screen's progress bar is split by weight: `init_core`'s six
/// fast steps count 1 each; the slow steps, done here with interrupts on,
/// count roughly by how long they take (installing ~65 KB over ATA PIO
/// takes ~1.5 s in QEMU, mounting a few milliseconds).
const MOUNT_WEIGHT: usize = 2;
const INSTALL_WEIGHT: usize = 12;
const BOOT_WEIGHT: usize = 6 + MOUNT_WEIGHT + INSTALL_WEIGHT;

/// Kernel stack of the thread that mounts the disk and installs programs.
/// The default 16 KiB (no guard page) is tight for the ext2 driver, which
/// used to run on the bootloader's larger stack.
const LOADER_STACK_SIZE: usize = 64 * 1024;

/// Set (and `CONSOLE_EVENTS` woken) once the boot screen is gone and the
/// console is ready for the shell.
static CONSOLE_READY: AtomicBool = AtomicBool::new(false);
static CONSOLE_EVENTS: WaitQueue = WaitQueue::new();

/// Boot. Only the fast part runs here, with interrupts off: `init_core`
/// (CPU tables, memory, heap, scheduler). The slow part (the disk) runs in
/// `shell_supervisor` with interrupts on, while `console_thread` keeps the
/// boot screen animating, so the screen never freezes.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::debug::set_level(LOG_LEVEL);
    // Keys typed during boot would be echoed over the animation.
    tty::set_input_enabled(false);
    boot_screen::begin(BOOT_WEIGHT);
    rust_os::init_core(boot_info, &mut |step, detail| boot_screen::step(step, detail));

    process_manager::create_thread(PID(0), console_thread);
    process_manager::create_thread_with_stack_size(PID(0), shell_supervisor, LOADER_STACK_SIZE);

    // From here on the timer runs, and so do the threads. kernel_main
    // itself never runs again.
    interrupts::enable();
    hlt_loop();
}

/// Kernel thread: the console's own UI. Plays the boot animation until
/// loading is done, clears it, switches to the terminal palette, and from
/// then on keeps the status bar up to date.
extern "C" fn console_thread() -> ! {
    boot_screen::play();
    boot_screen::finish();
    theme::apply();
    status_bar::install();
    tty::set_input_enabled(true);
    CONSOLE_READY.store(true, Ordering::SeqCst);
    CONSOLE_EVENTS.wake_all();
    status_bar::run()
}

/// Mount the root filesystem and install the programs, reporting each on
/// the boot screen. A failure is shown there, then the kernel panics.
fn load_system() {
    boot_screen::start_step("Root filesystem", MOUNT_WEIGHT);
    match rust_os::init_storage() {
        Ok(disk) => boot_screen::finish_step(format_args!(
            "ext2 on ATA disk, {} MiB, {} KiB blocks",
            disk.size_bytes() >> 20,
            disk.block_size / 1024
        )),
        Err(error) => {
            boot_screen::fail_step(format_args!("cannot mount: {:?}", error));
            panic!("filesystem initialization failed: {:?}", error);
        }
    }

    // Rewritten on every boot, so the copies on the persistent disk image
    // always match this build.
    boot_screen::start_step("Programs", INSTALL_WEIGHT);
    let installed = installed_programs();
    match programs::install(&installed) {
        Ok(()) => boot_screen::finish_step(format_args!(
            "{} installed in {}",
            installed.len(),
            programs::BIN_DIR
        )),
        Err(error) => {
            boot_screen::fail_step(format_args!("cannot install: {:?}", error));
            panic!("failed to install programs in /bin: {:?}", error);
        }
    }
}

/// Kernel thread: load the system (while the boot screen animates), then
/// keep one shell running: start `/bin/sh`, wait until it exits, then start
/// a new one.
extern "C" fn shell_supervisor() -> ! {
    load_system();
    boot_screen::loading_done();
    // The shell must not print before the boot screen is gone.
    CONSOLE_EVENTS.wait_until(|| CONSOLE_READY.load(Ordering::SeqCst).then_some(()));

    let path = Path::new_absolute(SHELL_PATH).expect("SHELL_PATH is absolute");
    loop {
        let pid = start_shell(&path);
        log_info!("started {} as PID({})", SHELL_PATH, pid.0);
        wait_for_exit(pid);
        log_info!("{} exited, restarting it", SHELL_PATH);
        println!("\n\x1b[33m[shell exited; starting a new one]\x1b[0m");
        sleep(RESTART_DELAY_TICKS);
    }
}

/// Load `/bin/sh` from the filesystem and start it with argv `["sh"]`, cwd
/// `/` and the console as fds 0–2.
fn start_shell(path: &Path) -> PID {
    let elf = programs::load(path)
        .unwrap_or_else(|error| panic!("cannot load {}: {:?}", path.as_str(), error));
    let (pid, _) = process_manager::spawn_process(SpawnArgs {
        parent: PID(0),
        name: String::from("sh"),
        elf: &elf,
        argv: &["sh"],
        cwd: Path::root(),
        stdio: [Some(stdin_file()), Some(stdout_file()), Some(stderr_file())],
    })
    .unwrap_or_else(|error| panic!("cannot start {}: {:?}", path.as_str(), error));
    pid
}

/// Block until process `pid` has exited: its `exit_code` is set, or it's
/// gone altogether.
///
/// The shell's parent is the kernel (PID 0). Nobody `wait4`s for such a
/// process, so it isn't kept as a zombie: the reaper usually frees it before
/// this thread runs again, which is why the exit code isn't reported.
/// `exit_current_thread` sets `exit_code` before it wakes `CHILD_EXITS`, so
/// no exit is missed. Known gap: if the PID were freed *and* reused before
/// this thread runs, it would wait for the wrong process. Only the shell's
/// orphaned children could spawn in that window.
fn wait_for_exit(pid: PID) {
    CHILD_EXITS.wait_until(|| {
        let pm = PROCESS_MANAGER.lock();
        let exited = pm
            .try_process(pid)
            .map_or(true, |process| process.exit_code.is_some());
        exited.then_some(())
    })
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    interrupts::without_interrupts(|| {
        serial_println!("{}", info);
    });
    rust_os::vga_buffer::print_panic(info);
    hlt_loop();
}

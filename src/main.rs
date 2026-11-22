#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::fs::devices::{stderr_file, stdin_file, stdout_file};
use rust_os::fs::Path;
use rust_os::process::process_manager::{self, SpawnArgs, PROCESS_MANAGER};
use rust_os::process::{CHILD_EXITS, PID};
use rust_os::programs;
use rust_os::scheduler::scheduler::sleep;
use rust_os::{debugln, hlt_loop, println, serial_println};
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

/// Kernel debug logging (`debugln!`, serial only). Off by default: it logs
/// every context switch, which buries the shell's output in the serial
/// terminal that `cargo run` shows. Turn it on to debug the kernel.
const DEBUG_LOG: bool = false;

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::debug::set_enabled(DEBUG_LOG);
    rust_os::init(boot_info);

    // Rewritten on every boot, so the copies on the persistent disk image
    // always match this build.
    rust_os::programs::install(&installed_programs()).expect("failed to install programs in /bin");
    debugln!(">>> installed {} programs in {}", installed_programs().len(), rust_os::programs::BIN_DIR);

    process_manager::create_thread(PID(0), shell_supervisor);

    interrupts::enable();
    hlt_loop();
}

/// Kernel thread: keep one shell running. Start `/bin/sh`, wait until it
/// exits, then start a new one.
extern "C" fn shell_supervisor() -> ! {
    let path = Path::new_absolute(SHELL_PATH).expect("SHELL_PATH is absolute");
    loop {
        let pid = start_shell(&path);
        debugln!(">>> started {} as PID({})", SHELL_PATH, pid.0);
        wait_for_exit(pid);
        println!("[shell exited; starting a new one]");
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
    hlt_loop();
}

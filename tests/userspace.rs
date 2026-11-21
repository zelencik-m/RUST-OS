#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(userspace_test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
mod support;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::fs::{ramfs::RamFs, File, FileSystem, InodeRef, OpenFlags, Vfs, VFS};
use rust_os::process::process_manager::{self, PROCESS_MANAGER};
use rust_os::process::thread::{ThreadState, TID};
use rust_os::process::PID;
use rust_os::scheduler::scheduler::SCHEDULER;
use rust_os::sync::IrqSafeMutex;
use x86_64::instructions::interrupts;

entry_point!(test_kernel_main);

/// The PIT runs at its default ~18.2 Hz, so this is roughly 110 s of guest
/// time. QEMU's emulated timer slows down when the host is busy (e.g. many
/// suites back to back, or x86 emulation on an ARM host), so keep this
/// generous; bootimage's own 300 s timeout still bounds a real hang.
const USER_TEST_TIMEOUT_TICKS: u64 = 2000;

#[repr(C, align(8))]
struct AlignedElf<const N: usize>([u8; N]);

macro_rules! elf {
    ($symbol:ident, $name:literal) => {
        static $symbol: AlignedElf<
            { include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".elf")).len() },
        > = AlignedElf(*include_bytes!(concat!(
            env!("OUT_DIR"),
            "/",
            $name,
            ".elf"
        )));
    };
}

elf!(ELF_LOADER, "elf_loader_test");
elf!(SYSCALL, "syscall_test");
elf!(VFS_TEST, "vfs_test");
elf!(FS_TREE, "fs_tree_test");
elf!(RING3_EXCEPTION, "ring3_exception_test");
elf!(NX_EXEC, "nx_exec_test");
elf!(FS_OPS, "fs_ops_test");
elf!(TTY_TEST, "tty_test");

#[derive(Clone)]
struct UserCase {
    name: &'static str,
    tid: TID,
    stdout: InodeRef,
    stderr: InodeRef,
    success_marker: &'static str,
    /// Keep this case off the run queue until the named case has finished.
    /// Use it for programs that would race another one on the shared RamFs.
    start_after: Option<&'static str>,
    /// Keys to type on the console once this case's thread blocks (in a
    /// `read(0)`). Typed once.
    input: Option<&'static str>,
}

struct RunnerState {
    cases: Vec<UserCase>,
    /// `(tid, dependency)` for deferred cases that have not been started yet.
    deferred: Vec<(TID, &'static str)>,
    /// `(tid, keys)` for cases whose input has not been typed yet.
    pending_input: Vec<(TID, &'static str)>,
}

static RUNNER: IrqSafeMutex<Option<RunnerState>> = IrqSafeMutex::new(None);

fn userspace_test_runner(tests: &[&dyn Fn()]) {
    assert_eq!(
        tests.len(),
        1,
        "userspace runner expects one orchestration test"
    );
    tests[0]();
}

fn read_inode(inode: &InodeRef) -> Vec<u8> {
    let length = inode.len().expect("capture inode length failed") as usize;
    let mut bytes = alloc::vec![0; length];
    let read = inode
        .read_at(0, &mut bytes)
        .expect("capture inode read failed");
    bytes.truncate(read);
    bytes
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

extern "C" fn user_monitor() -> ! {
    loop {
        let (cases, elapsed) = interrupts::without_interrupts(|| {
            let state = RUNNER.lock();
            let cases = state
                .as_ref()
                .expect("userspace monitor state missing")
                .cases
                .clone();
            let ticks = SCHEDULER.lock().tick_count();
            (cases, ticks)
        });

        if elapsed > USER_TEST_TIMEOUT_TICKS {
            for case in &cases {
                let (out, err) = interrupts::without_interrupts(|| {
                    (read_inode(&case.stdout), read_inode(&case.stderr))
                });
                rust_os::serial_println!(
                    "{} timed out; stdout={:?}; stderr={:?}",
                    case.name,
                    out,
                    err
                );
            }
            panic!("user ELF smoke tests timed out");
        }

        let mut finished = true;
        let mut done: Vec<&'static str> = Vec::new();
        for case in &cases {
            // Capture output and completion together: a preemption between
            // these checks could otherwise hide a final failure message.
            let (out, err, no_longer_scheduled) = interrupts::without_interrupts(|| {
                let scheduler = SCHEDULER.lock();
                let finished =
                    scheduler.get_current() != Some(case.tid) && !scheduler.is_runnable(case.tid);
                (read_inode(&case.stdout), read_inode(&case.stderr), finished)
            });
            let marker_seen = bytes_contain(&out, case.success_marker.as_bytes());
            let failed_output = if case.name == "fs_tree_test" {
                [
                    b"[cannot".as_slice(),
                    b"[getdents64",
                    b"[invalid",
                    b"[maximum",
                    b"[path too",
                    b"failed",
                ]
                .iter()
                .any(|marker| bytes_contain(&out, marker))
            } else {
                false
            };
            if !err.is_empty() || failed_output {
                rust_os::serial_println!(
                    "{} failed; stdout={:?}; stderr={:?}",
                    case.name,
                    out,
                    err
                );
                panic!("user ELF reported failure");
            }
            if marker_seen && no_longer_scheduled {
                done.push(case.name);
            } else {
                finished = false;
            }
        }

        interrupts::without_interrupts(|| {
            let mut state = RUNNER.lock();
            let state = state.as_mut().expect("userspace monitor state missing");
            state.deferred.retain(|&(tid, after)| {
                if done.contains(&after) {
                    SCHEDULER.lock().add_runnable(tid);
                    false
                } else {
                    true
                }
            });
        });

        // Type pending input for programs now blocked in read(0). The keys
        // are pushed after every lock is dropped: push_key takes the TTY,
        // SCHEDULER and PROCESS_MANAGER locks itself.
        let mut to_type: Vec<&'static str> = Vec::new();
        interrupts::without_interrupts(|| {
            let mut state = RUNNER.lock();
            let state = state.as_mut().expect("userspace monitor state missing");
            let pm = PROCESS_MANAGER.lock();
            state.pending_input.retain(|&(tid, keys)| {
                let blocked =
                    pm.thread_exists(tid) && pm.get_thread_state(tid) == ThreadState::Blocked;
                if blocked {
                    to_type.push(keys);
                }
                !blocked
            });
        });
        for keys in to_type {
            for c in keys.chars() {
                rust_os::tty::push_key(pc_keyboard::DecodedKey::Unicode(c));
            }
        }

        if finished {
            for case in &cases {
                support::report_pass(case.name);
            }
            support::finish_suite();
        }
        rust_os::scheduler::scheduler::sleep(1);
    }
}

fn add_case(name: &'static str, image: &[u8], marker: &'static str, root: &InodeRef) -> UserCase {
    let (pid, tid) = process_manager::spawn_elf(String::from(name), image)
        .unwrap_or_else(|_| panic!("failed to load user ELF {}", name));
    let stdout: InodeRef = root
        .create_child(
            &alloc::format!("{name}.stdout"),
            rust_os::fs::FileType::Regular,
        )
        .unwrap();
    let stderr: InodeRef = root
        .create_child(
            &alloc::format!("{name}.stderr"),
            rust_os::fs::FileType::Regular,
        )
        .unwrap();
    {
        let mut pm = PROCESS_MANAGER.lock();
        let process = pm.process_mut(pid);
        process.fd_table[1] = Some(File::new(stdout.clone(), OpenFlags::write_only()).unwrap());
        process.fd_table[2] = Some(File::new(stderr.clone(), OpenFlags::write_only()).unwrap());
    }
    UserCase {
        name,
        tid,
        stdout,
        stderr,
        success_marker: marker,
        start_after: None,
        input: None,
    }
}

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    test_main();
    rust_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

#[test_case]
fn run_user_elf_smoke_programs() {
    let capture_fs = Arc::new(RamFs::new());
    let root = capture_fs.root();
    *VFS.lock() = Some(Vfs::new(capture_fs));

    let mut cases = Vec::new();
    cases.push(add_case(
        "elf_loader_test",
        &ELF_LOADER.0,
        "ELF loader test passed",
        &root,
    ));
    cases.push(add_case(
        "syscall_test",
        &SYSCALL.0,
        "Hello syscalls!",
        &root,
    ));
    cases.push(add_case("vfs_test", &VFS_TEST.0, "VFS test passed", &root));
    cases.push(add_case(
        "fs_tree_test",
        &FS_TREE.0,
        "fs_tree_test: passed",
        &root,
    ));
    // fs_tree_test walks the whole tree from `/`. If fs_ops_test removed a
    // directory mid-walk, fs_tree_test would fail with "[cannot open ...]".
    let mut fs_ops = add_case("fs_ops_test", &FS_OPS.0, "fs_ops_test: passed", &root);
    fs_ops.start_after = Some("fs_tree_test");
    cases.push(fs_ops);
    // Types its stdin once it blocks in read(0): a line, then Ctrl-D (EOF).
    let mut tty_case = add_case("tty_test", &TTY_TEST.0, "tty_test: passed", &root);
    tty_case.input = Some("hello\n\u{4}");
    cases.push(tty_case);
    cases.push(add_case(
        "ring3_exception_test",
        &RING3_EXCEPTION.0,
        "Triggering a Ring-3 page fault",
        &root,
    ));
    cases.push(add_case(
        "nx_exec_test",
        &NX_EXEC.0,
        "Attempting to execute non-executable stack memory",
        &root,
    ));

    support::begin_suite("userspace", cases.len());

    // The normal kernel init thread runs the interactive executor. Replace its
    // queued entry with a monitor so this standalone QEMU test has a finite end.
    let monitor_tid = process_manager::create_thread(PID(0), user_monitor);
    *RUNNER.lock() = Some(RunnerState {
        cases: cases.clone(),
        deferred: cases
            .iter()
            .filter_map(|case| case.start_after.map(|after| (case.tid, after)))
            .collect(),
        pending_input: cases
            .iter()
            .filter_map(|case| case.input.map(|keys| (case.tid, keys)))
            .collect(),
    });
    {
        let mut scheduler = SCHEDULER.lock();
        scheduler.clear_runnables();
        scheduler.add_runnable(monitor_tid);
        for case in cases.iter().filter(|case| case.start_after.is_none()) {
            scheduler.add_runnable(case.tid);
        }
    }
    interrupts::enable();
    rust_os::scheduler::scheduler::yield_now();
    panic!("userspace monitor returned unexpectedly");
}

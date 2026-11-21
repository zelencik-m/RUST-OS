//! Process lifecycle with real ring-3 programs: `spawn_process`, exit codes,
//! zombies, `wait_child`, re-parenting and Ctrl-C kills.
//!
//! One orchestration test (like `tty_blocking`): a monitor kernel thread
//! spawns `proc_child` (tests/user_programs/proc_child.c) in different modes
//! and checks the process table and exit codes. Each numbered step is
//! reported as its own PASS line.
//!
//! Most children get a *user* parent `P`: a process created with
//! `create_user_process` that has no threads. It never exits, so it counts
//! as a live user parent, and the monitor calls `wait_child(P, child)` on
//! its behalf. The VFS is a RamFs with `/bin/proc_child`, which the `orphan`
//! and `waiter` modes spawn from ring 3.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(lifecycle_test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use pc_keyboard::DecodedKey;
use rust_os::fs::devices::stdin_file;
use rust_os::fs::ramfs::RamFs;
use rust_os::fs::{File, FileSystem, FileType, InodeRef, OpenFlags, Path, Vfs, VFS};
use rust_os::process::process_manager::{
    self, create_user_process, request_kill, spawn_process, wait_child, SpawnArgs, WaitError,
    PROCESS_MANAGER,
};
use rust_os::process::thread::{ThreadState, TID};
use rust_os::process::{EXIT_FAULT, EXIT_INTERRUPTED, PID};
use rust_os::scheduler::scheduler::{sleep, yield_now, SCHEDULER};
use rust_os::sync::IrqSafeMutex;
use rust_os::tty::{self, TtyMode};
use x86_64::instructions::interrupts;

/// Per-wait bound. The PIT runs at ~18.2 Hz, so this is about 16 s.
const WAIT_TIMEOUT_TICKS: u64 = 300;
const STEPS: usize = 12;

#[repr(C, align(8))]
struct AlignedElf<const N: usize>([u8; N]);

static PROC_CHILD: AlignedElf<
    { include_bytes!(concat!(env!("OUT_DIR"), "/proc_child.elf")).len() },
> = AlignedElf(*include_bytes!(concat!(env!("OUT_DIR"), "/proc_child.elf")));

entry_point!(test_kernel_main);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

fn lifecycle_test_runner(tests: &[&dyn Fn()]) {
    assert_eq!(tests.len(), 1, "process_lifecycle expects one orchestration test");
    tests[0]();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The RamFs root, for the children's capture files.
static ROOT: IrqSafeMutex<Option<InodeRef>> = IrqSafeMutex::new(None);
static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(0);

struct Child {
    pid: PID,
    tid: TID,
    stdout: InodeRef,
    stderr: InodeRef,
}

fn capture_file(kind: &str) -> InodeRef {
    let n = NEXT_CAPTURE.fetch_add(1, Ordering::SeqCst);
    let root = ROOT.lock().clone().expect("RamFs root not set");
    root.create_child(&alloc::format!("capture{n}.{kind}"), FileType::Regular)
        .expect("create capture file")
}

/// Spawn `proc_child` with `argv` (argv[0] is added) as a child of `parent`.
fn spawn_child_in(parent: PID, cwd: &str, args: &[&str]) -> Child {
    let mut argv: Vec<&str> = alloc::vec!["proc_child"];
    argv.extend_from_slice(args);
    let stdout = capture_file("stdout");
    let stderr = capture_file("stderr");
    let (pid, tid) = spawn_process(SpawnArgs {
        parent,
        name: String::from("proc_child"),
        elf: &PROC_CHILD.0,
        argv: &argv,
        cwd: Path::new_absolute(cwd).unwrap(),
        stdio: [
            Some(stdin_file()),
            Some(File::new(stdout.clone(), OpenFlags::write_only()).unwrap()),
            Some(File::new(stderr.clone(), OpenFlags::write_only()).unwrap()),
        ],
    })
    .unwrap_or_else(|error| panic!("spawn_process({:?}) failed: {:?}", args, error));
    Child { pid, tid, stdout, stderr }
}

fn spawn_child(parent: PID, args: &[&str]) -> Child {
    spawn_child_in(parent, "/", args)
}

fn read_all(inode: &InodeRef) -> Vec<u8> {
    let mut bytes = alloc::vec![0u8; inode.len().unwrap() as usize];
    let n = inode.read_at(0, &mut bytes).unwrap();
    bytes.truncate(n);
    bytes
}

fn assert_no_errors(child: &Child) {
    let err = read_all(&child.stderr);
    assert!(
        err.is_empty(),
        "child {:?} wrote to stderr: {}",
        child.pid,
        core::str::from_utf8(&err).unwrap_or("<not UTF-8>")
    );
}

/// Poll `condition` once per tick until it holds; panic after the timeout.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = SCHEDULER.lock().tick_count() + WAIT_TIMEOUT_TICKS;
    while !condition() {
        if SCHEDULER.lock().tick_count() > deadline {
            panic!("timed out waiting for: {}", what);
        }
        sleep(1);
    }
}

fn exists(pid: PID) -> bool {
    PROCESS_MANAGER.lock().try_process(pid).is_some()
}

fn exit_code(pid: PID) -> Option<i32> {
    PROCESS_MANAGER.lock().try_process(pid).and_then(|p| p.exit_code)
}

fn parent_of(pid: PID) -> Option<PID> {
    PROCESS_MANAGER.lock().try_process(pid).and_then(|p| p.parent())
}

fn children_of(pid: PID) -> Vec<PID> {
    PROCESS_MANAGER.lock().process(pid).get_children().clone()
}

fn thread_state(tid: TID) -> Option<ThreadState> {
    let pm = PROCESS_MANAGER.lock();
    pm.thread_exists(tid).then(|| pm.get_thread_state(tid))
}

/// The pid an `orphan`/`waiter` child printed as "child=N".
fn grandchild_pid(child: &Child) -> PID {
    wait_for("the child to print its child's pid", || {
        read_all(&child.stdout).ends_with(b"\n")
    });
    let out = read_all(&child.stdout);
    let text = core::str::from_utf8(&out).unwrap();
    let number = text
        .trim()
        .strip_prefix("child=")
        .unwrap_or_else(|| panic!("unexpected output {:?}", text));
    PID(number.parse().expect("child pid is not a number"))
}

fn ctrl_c() {
    tty::push_key(DecodedKey::Unicode('\u{3}'));
}

fn pass(step: &str) {
    support::report_pass(step);
}

// A kernel thread that calls wait_child, for the blocking test.
static WAIT_PARENT: AtomicU64 = AtomicU64::new(0);
static WAIT_CHILD: AtomicU64 = AtomicU64::new(0);
static WAIT_RESULT: IrqSafeMutex<Option<Result<i32, WaitError>>> = IrqSafeMutex::new(None);

extern "C" fn waiting_thread() -> ! {
    let parent = PID(WAIT_PARENT.load(Ordering::SeqCst));
    let child = PID(WAIT_CHILD.load(Ordering::SeqCst));
    let result = wait_child(parent, child);
    *WAIT_RESULT.lock() = Some(result);
    loop {
        sleep(1_000_000);
    }
}

// ---------------------------------------------------------------------------
// The steps
// ---------------------------------------------------------------------------

extern "C" fn monitor() -> ! {
    let p = create_user_process(String::from("parent")).expect("create parent");

    // 1. spawn_process: parent/child links, cwd, stdio and argv.
    let child = spawn_child_in(p, "/bin", &["echo", "a", "b c"]);
    assert_eq!(parent_of(child.pid), Some(p), "child's parent");
    assert!(children_of(p).contains(&child.pid), "parent's children list");
    {
        let pm = PROCESS_MANAGER.lock();
        let process = pm.process(child.pid);
        assert_eq!(process.cwd().as_str(), "/bin", "child's cwd");
        assert!(
            process.fd_table[0].as_ref().unwrap().inode().is_terminal(),
            "fd 0 must be the console"
        );
    }
    assert_eq!(wait_child(p, child.pid), Ok(0));
    assert_eq!(read_all(&child.stdout), b"argv:proc_child|echo|a|b c\n");
    assert_no_errors(&child);
    pass("process_lifecycle::spawn_process_sets_links_cwd_stdio_and_argv");

    // 2. Exit codes: exit(n), and a fatal fault.
    for (args, expected) in [
        (&["exit", "7"][..], 7),
        (&["exit", "300"][..], 300 & 0xff),
        (&["exit", "0"][..], 0),
        (&["fault"][..], EXIT_FAULT),
    ] {
        let child = spawn_child(p, args);
        assert_eq!(wait_child(p, child.pid), Ok(expected), "exit code for {:?}", args);
        assert_no_errors(&child);
    }
    pass("process_lifecycle::exit_codes_reach_wait_child");

    // 3. An exited child stays a zombie (even after its thread is reaped)
    //    until the parent collects it; then its slot is freed.
    let child = spawn_child(p, &["exit", "5"]);
    wait_for("the child's thread to be reaped", || thread_state(child.tid).is_none());
    sleep(2);
    assert!(exists(child.pid), "zombie was freed before its parent collected it");
    assert_eq!(exit_code(child.pid), Some(5), "zombie's exit code");
    assert_eq!(wait_child(p, child.pid), Ok(5));
    assert!(!exists(child.pid), "collected zombie must be freed");
    assert!(!children_of(p).contains(&child.pid), "collected child still listed");
    assert_eq!(wait_child(p, child.pid), Err(WaitError::NotChild), "second wait");
    pass("process_lifecycle::zombie_is_kept_until_collected");

    // 4. Only your own, not yet collected, children can be waited for.
    assert_eq!(wait_child(p, PID(9999)), Err(WaitError::NotChild));
    assert_eq!(wait_child(p, p), Err(WaitError::NotChild));
    let other_parent = create_user_process(String::from("other")).expect("create parent");
    let theirs = spawn_child(other_parent, &["exit", "1"]);
    assert_eq!(wait_child(p, theirs.pid), Err(WaitError::NotChild));
    assert_eq!(wait_child(other_parent, theirs.pid), Ok(1));
    pass("process_lifecycle::wait_child_rejects_non_children");

    // 5. wait_child blocks while the child runs, and wakes when it exits.
    let child = spawn_child(p, &["nap", "15", "9"]);
    WAIT_PARENT.store(p.0, Ordering::SeqCst);
    WAIT_CHILD.store(child.pid.0, Ordering::SeqCst);
    let waiter = process_manager::create_thread(PID(0), waiting_thread);
    wait_for("the waiter to block", || thread_state(waiter) == Some(ThreadState::Blocked));
    assert!(WAIT_RESULT.lock().is_none(), "wait_child returned while the child runs");
    assert_eq!(exit_code(child.pid), None);
    wait_for("the waiter to get the exit code", || WAIT_RESULT.lock().is_some());
    assert_eq!(*WAIT_RESULT.lock(), Some(Ok(9)));
    pass("process_lifecycle::wait_child_blocks_until_exit");

    // 6. Kernel-parented processes are freed as soon as they exit: nobody
    //    will ever wait for them.
    let child = spawn_child(PID(0), &["exit", "1"]);
    wait_for("the kernel-parented child to be freed", || !exists(child.pid));
    pass("process_lifecycle::kernel_children_are_freed_at_exit");

    // 7. When a parent exits, its live children are re-parented to PID 0,
    //    and freed when they exit.
    let orphaner = spawn_child(p, &["orphan", "nap", "90", "0"]);
    let grandchild = grandchild_pid(&orphaner);
    assert_eq!(parent_of(grandchild), Some(orphaner.pid));
    assert_eq!(wait_child(p, orphaner.pid), Ok(0));
    assert_eq!(exit_code(grandchild), None, "the grandchild should still be napping");
    assert_eq!(parent_of(grandchild), Some(PID(0)), "orphan not re-parented to PID 0");
    wait_for("the orphan to be freed after it exits", || !exists(grandchild));
    assert_no_errors(&orphaner);
    pass("process_lifecycle::orphans_are_reparented_to_the_kernel");

    // 8. A zombie whose parent exits is freed too (nobody can collect it).
    let orphaner = spawn_child(p, &["orphan", "exit", "4"]);
    let grandchild = grandchild_pid(&orphaner);
    wait_for("the grandchild to become a zombie", || exit_code(grandchild) == Some(4));
    assert!(exists(grandchild), "zombie of a live user parent was freed");
    assert_eq!(wait_child(p, orphaner.pid), Ok(0));
    wait_for("the orphaned zombie to be freed", || !exists(grandchild));
    pass("process_lifecycle::orphaned_zombies_are_freed");

    // 9. Ctrl-C kills the foreground child wherever it is: running user code
    //    (timer safe point), blocked in read(0), or sleeping.
    for (args, state) in [
        (&["spin"][..], None),
        (&["read"][..], Some(ThreadState::Blocked)),
        (&["sleep"][..], Some(ThreadState::Sleeping)),
    ] {
        let child = spawn_child(p, args);
        match state {
            Some(state) => wait_for("the child to block", || thread_state(child.tid) == Some(state)),
            None => sleep(5),
        }
        tty::set_foreground(Some(child.pid));
        ctrl_c();
        assert_eq!(wait_child(p, child.pid), Ok(EXIT_INTERRUPTED), "Ctrl-C on {:?}", args);
        tty::set_foreground(None);
        assert_no_errors(&child);
    }
    pass("process_lifecycle::ctrl_c_kills_the_foreground_child");

    // 10. Ctrl-C only kills in cooked mode, and only the foreground.
    let child = spawn_child(p, &["spin"]);
    sleep(3);
    ctrl_c(); // no foreground
    tty::set_foreground(Some(child.pid));
    assert_eq!(tty::set_mode(TtyMode::Raw), TtyMode::Cooked);
    ctrl_c(); // raw mode: just a byte
    let mut buf = [0u8; 8];
    assert_eq!(tty::read(&mut buf), Ok(1));
    assert_eq!(buf[0], 0x03, "raw Ctrl-C must be readable as 0x03");
    assert_eq!(tty::set_mode(TtyMode::Cooked), TtyMode::Raw);
    sleep(5);
    assert!(exists(child.pid) && exit_code(child.pid).is_none(), "child was killed early");
    ctrl_c();
    assert_eq!(wait_child(p, child.pid), Ok(EXIT_INTERRUPTED));
    tty::set_foreground(None);
    pass("process_lifecycle::ctrl_c_needs_cooked_mode_and_a_foreground");

    // 11. A process killed while it waits in wait4 exits with 130, and its
    //     still-running child is orphaned.
    let waiter = spawn_child(p, &["waiter", "sleep"]);
    let sleeper = grandchild_pid(&waiter);
    wait_for("the waiter to block in wait4", || {
        thread_state(waiter.tid) == Some(ThreadState::Blocked)
    });
    tty::set_foreground(Some(waiter.pid));
    ctrl_c();
    assert_eq!(wait_child(p, waiter.pid), Ok(EXIT_INTERRUPTED));
    tty::set_foreground(None);
    assert_eq!(parent_of(sleeper), Some(PID(0)), "sleeper not re-parented");
    request_kill(sleeper, 77);
    wait_for("the orphaned sleeper to be killed and freed", || !exists(sleeper));
    assert_no_errors(&waiter);
    pass("process_lifecycle::kill_interrupts_wait4");

    // 12. Kill requests for the kernel or a missing process are ignored.
    request_kill(PID(0), EXIT_INTERRUPTED);
    request_kill(PID(9999), EXIT_INTERRUPTED);
    assert_eq!(PROCESS_MANAGER.lock().process(PID(0)).pending_kill, None);
    sleep(2);
    pass("process_lifecycle::bogus_kill_requests_are_ignored");

    support::finish_suite();
}

#[test_case]
fn run_process_lifecycle_steps() {
    support::begin_suite("process_lifecycle", STEPS);

    let ramfs = Arc::new(RamFs::new());
    let root = ramfs.root();
    let bin = root.create_child("bin", FileType::Directory).unwrap();
    let elf = bin.create_child("proc_child", FileType::Regular).unwrap();
    elf.write_at(0, &PROC_CHILD.0).unwrap();
    *ROOT.lock() = Some(root);
    *VFS.lock() = Some(Vfs::new(ramfs));

    let monitor_tid = process_manager::create_thread(PID(0), monitor);
    // Replace the boot-time init thread (which runs the executor forever).
    {
        let mut scheduler = SCHEDULER.lock();
        scheduler.clear_runnables();
        scheduler.add_runnable(monitor_tid);
    }
    interrupts::enable();
    yield_now();
    panic!("process_lifecycle: boot context resumed unexpectedly");
}

mod support;

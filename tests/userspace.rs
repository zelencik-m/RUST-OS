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
use rust_os::fs::devices::stdin_file;
use rust_os::fs::{ramfs::RamFs, File, FileSystem, InodeRef, OpenFlags, Path, Vfs, VFS};
use rust_os::process::process_manager::{self, SpawnArgs, PROCESS_MANAGER};
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
elf!(SPAWN_TEST, "spawn_test");
elf!(PROC_CHILD, "proc_child");
elf!(ULIB_TEST, "ulib_test");
elf!(HELLO, "hello");
elf!(SH, "sh");

/// `sh_script`'s stdin. Every command's expected output is in
/// `SH_SCRIPT_OUTPUT`, in order.
const SH_SCRIPT: &str = "\
# rust-os shell script test (tests/userspace.rs)
mkdir p5
cd p5
pwd
touch f
ls
mkdir d d/e
touch d/g d/e/h
rm d
ls d
echo \"a  b\" c \"\" end   # a comment
echo a\"b c\"d
rmdir nope
nosuch
hello x
/bin/ulib_test ret 3
cat /sh_note.txt
cd /
rm -r p5
ls p5
cd /p5
echo \"unterminated
exit 4
echo unreachable
";

/// What `sh_script` must print (stdout and stderr are one file there).
const SH_SCRIPT_OUTPUT: &[&str] = &[
    "/p5\n",
    "f\n",
    "rm: d: Is a directory\n",
    "e/  g\n",
    "a  b c  end\n",
    "ab cd\n",
    "rmdir: nope: No such file or directory\n",
    "sh: nosuch: command not found\n",
    "hello: argc=2 x\n",
    "[exit 3]\n",
    "note contents\n",
    "ls: p5: No such file or directory\n",
    "cd: /p5: No such file or directory\n",
    "sh: unterminated quote\n",
];

/// Keys typed into `sh_interactive`. Raw mode passes ESC through, so the
/// arrow-key sequences can be typed as plain characters.
const SH_KEYS: &str = concat!(
    "eho hi",
    "\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D", // 5 x Left: the cursor is after the 'e'
    "c\n",                               // -> "echo hi"
    "\x1b[A\n",                          // Up: "echo hi" again
    "hello z\n",                         // a program: cooked mode + foreground
    "exit\n",
);

/// The interactive shell's prompt at `/`: the cwd in teal (bright blue),
/// then a green `►`.
macro_rules! sh_prompt {
    () => {
        "\x1b[1;34m/\x1b[0m \x1b[1;32m\u{25ba}\x1b[0m "
    };
}

/// What `sh_interactive` must print. The editor redraws the row as
/// `\r prompt line ESC[K` (erase the rest of the row) and moves back to
/// the cursor with `\b`.
const SH_INTERACTIVE_OUTPUT: &[&str] = &[
    // The banner box (its title has one color per letter), then the first
    // prompt.
    "\u{250c}\u{2500}[ \x1b[1;97mR\x1b[1;96mU\x1b[1;96mS\x1b[1;94mT",
    concat!("\u{2518}\x1b[0m\n\n", sh_prompt!()),
    // After inserting 'c': cursor at 2 of 7, so 5 x \b. Then Enter.
    concat!("\r", sh_prompt!(), "echo hi\x1b[K\x08\x08\x08\x08\x08\nhi\n"),
    // The history line, cursor at its end.
    concat!("\r", sh_prompt!(), "echo hi\x1b[K\nhi\n"),
    "hello: argc=2 z\n",
];

#[derive(Clone)]
struct UserCase {
    name: &'static str,
    tid: TID,
    stdout: InodeRef,
    stderr: InodeRef,
    /// Strings that must all appear in stdout, in this order (each one
    /// after the end of the previous).
    markers: Vec<&'static str>,
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
    find(haystack, needle).is_some()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn contains_in_order(haystack: &[u8], markers: &[&str]) -> bool {
    let mut rest = haystack;
    for marker in markers {
        match find(rest, marker.as_bytes()) {
            Some(at) => rest = &rest[at + marker.len()..],
            None => return false,
        }
    }
    true
}

/// Captured output for failure messages: text, with control characters
/// escaped.
fn show(bytes: &[u8]) -> String {
    alloc::format!("{:?}", String::from_utf8_lossy(bytes))
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
                    "{} timed out; stdout={}; stderr={}",
                    case.name,
                    show(&out),
                    show(&err)
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
            let marker_seen = contains_in_order(&out, &case.markers);
            let failed_output = if case.name == "sh_script" {
                // `exit 4` must stop the script.
                bytes_contain(&out, b"unreachable")
            } else if case.name == "fs_tree_test" {
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
                    "{} failed; stdout={}; stderr={}",
                    case.name,
                    show(&out),
                    show(&err)
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
    add_case_with_argv(name, image, &[name], marker, root)
}

/// Start `image` with this argv, the console as stdin, and stdout/stderr
/// captured in `/<name>.stdout` and `.stderr`.
fn add_case_with_argv(
    name: &'static str,
    image: &[u8],
    argv: &[&str],
    marker: &'static str,
    root: &InodeRef,
) -> UserCase {
    add_case_with(
        name,
        image,
        argv,
        alloc::vec![marker],
        CaseStdio::default(),
        root,
    )
}

/// How a case's fds 0 and 2 are set up (fd 1 is always the capture file).
#[derive(Default)]
struct CaseStdio {
    /// fd 0. `None`: the console.
    stdin: Option<File>,
    /// fd 2 is the *same open file* as fd 1 (shared offset), so error
    /// messages land in stdout in order with the rest. The runner fails a
    /// case on any stderr output, so this is how a test checks expected
    /// errors.
    stderr_to_stdout: bool,
}

/// Start `image` (kernel-parented, cwd `/`) with this argv and stdio. Its
/// stdout must contain `markers` in order.
fn add_case_with(
    name: &'static str,
    image: &[u8],
    argv: &[&str],
    markers: Vec<&'static str>,
    stdio: CaseStdio,
    root: &InodeRef,
) -> UserCase {
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
    let stdout_file = File::new(stdout.clone(), OpenFlags::write_only()).unwrap();
    let stderr_file = if stdio.stderr_to_stdout {
        stdout_file.share()
    } else {
        File::new(stderr.clone(), OpenFlags::write_only()).unwrap()
    };
    let (_, tid) = process_manager::spawn_process(SpawnArgs {
        parent: PID(0),
        name: String::from(name),
        elf: image,
        argv,
        cwd: Path::root(),
        stdio: [
            Some(stdio.stdin.unwrap_or_else(stdin_file)),
            Some(stdout_file),
            Some(stderr_file),
        ],
    })
    .unwrap_or_else(|_| panic!("failed to load user ELF {}", name));
    UserCase {
        name,
        tid,
        stdout,
        stderr,
        markers,
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
    // spawn_test starts /bin/proc_child, and ulib_test starts itself.
    let bin = root
        .create_child("bin", rust_os::fs::FileType::Directory)
        .unwrap();
    for (name, image) in [
        ("proc_child", &PROC_CHILD.0[..]),
        ("ulib_test", &ULIB_TEST.0[..]),
        ("hello", &HELLO.0[..]),
    ] {
        bin.create_child(name, rust_os::fs::FileType::Regular)
            .unwrap()
            .write_at(0, image)
            .unwrap();
    }

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
    // Its children inherit stdout, so their output comes before the final
    // line. It reads nothing itself, but its `read` child must not take
    // tty_test's input, so it starts after tty_test. The runner types
    // Ctrl-C when it first blocks (in wait4 for that child).
    let mut spawn_case = add_case(
        "spawn_test",
        &SPAWN_TEST.0,
        "argv:proc_child|echo|hi\ncwd:/bin\nspawn_test: passed\n",
        &root,
    );
    spawn_case.start_after = Some("tty_test");
    spawn_case.input = Some("\u{3}");
    cases.push(spawn_case);
    // Creates and removes /ulib_test.out, so it starts after the two
    // programs that walk and change the tree.
    let mut ulib_case = add_case("ulib_test", &ULIB_TEST.0, "ulib_test: passed\n", &root);
    ulib_case.start_after = Some("fs_ops_test");
    cases.push(ulib_case);
    // The whole output: crt0 passes argc/argv to main.
    cases.push(add_case_with_argv(
        "hello",
        &HELLO.0,
        &["hello", "a", "b"],
        "hello: argc=3 a b\n",
        &root,
    ));
    // The shell in script mode: stdin is a file, so tty_mode fails with
    // ENOTTY. stderr goes into stdout so the expected errors are checked in
    // order. It creates and removes /p5, so it starts after the tree tests.
    let script = root
        .create_child("sh_script.in", rust_os::fs::FileType::Regular)
        .unwrap();
    script.write_at(0, SH_SCRIPT.as_bytes()).unwrap();
    root.create_child("sh_note.txt", rust_os::fs::FileType::Regular)
        .unwrap()
        .write_at(0, b"note contents\n")
        .unwrap();
    let mut sh_script = add_case_with(
        "sh_script",
        &SH.0,
        &["sh"],
        SH_SCRIPT_OUTPUT.to_vec(),
        CaseStdio {
            stdin: Some(File::new(script, OpenFlags::read_only()).unwrap()),
            stderr_to_stdout: true,
        },
        &root,
    );
    sh_script.start_after = Some("fs_ops_test");
    cases.push(sh_script);
    // The shell on the console: the line editor, history, and running a
    // program. It switches the console to raw mode, so it waits until the
    // other console readers (tty_test, then spawn_test) are done.
    let mut sh_interactive = add_case_with(
        "sh_interactive",
        &SH.0,
        &["sh"],
        SH_INTERACTIVE_OUTPUT.to_vec(),
        CaseStdio::default(),
        &root,
    );
    sh_interactive.start_after = Some("spawn_test");
    sh_interactive.input = Some(SH_KEYS);
    cases.push(sh_interactive);
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

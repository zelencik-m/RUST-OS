# rust-os

A modular kernel with processes, threads, and a FIFO scheduler supporting time-slice preemption and sleep queues.
It boots into a small Unix-like shell running in user mode.

## Using the shell

`cargo run` boots the kernel in QEMU and starts `/bin/sh`. Type in the QEMU
window; the terminal that ran `cargo run` shows a copy of the output (serial).

```
rust-os shell - type 'help'
/$ mkdir projects
/$ cd projects
/projects$ touch notes.txt
/projects$ ls
notes.txt
/projects$ cd ..
/$ ls /bin
sh  hello  thread_b  thread_c
/$ hello a b
hello: argc=3 a b
/$ thread_b
executing ELF thread B
...
^C
[exit 130]
/$ rm -r projects
```

- **Builtins:** `cd [dir]`, `pwd`, `ls [path]`, `mkdir`, `rmdir`, `rm [-r]`,
  `touch`, `cat`, `echo`, `clear`, `help`, `exit [status]`.
- **Programs:** any other name runs `/bin/<name>`; a name containing `/` is
  used as a path. The shell waits for the program and prints `[exit N]` if
  it exits with a status other than 0. Ctrl-C stops it (status 130).
- **Parsing:** words are separated by spaces; `"double quotes"` keep spaces;
  `#` starts a comment. No pipes or redirection.
- **Line editing:** ←/→ move the cursor, Backspace deletes, ↑/↓ walk the last
  16 lines, Ctrl-C drops the line, Ctrl-L clears the screen. A line has to fit
  on one screen row.
- If the shell exits, the kernel starts a new one.
- The root filesystem is `target/ext2-disk.img`, a copy of `ext2.img` that is
  kept between runs. Files you create persist across reboots; delete the file
  to start from a clean disk. The kernel reinstalls the programs in `/bin` on
  every boot.

Programs are C files built against the small libc in `user_programs/ulib/`
(`crt0`, syscall wrappers, string functions, `printf`); see `user_programs/hello.c`.

## Kernel tests

All test cases live under `tests/`. Run the no-std integration suites with
`cargo test` (requires nightly Rust, `bootimage`, QEMU, Clang, and the ext2 image
configured in `.cargo/config.toml` and `Cargo.toml`). Each Rust test file boots
its own test kernel. Run one suite with `cargo test --test <file_stem>`.

Debug logging (serial only) is off by default; set `DEBUG_LOG` in
`src/main.rs` to turn it on for a normal boot. Test kernels keep it disabled.
Every suite prints aligned per-test results followed by passed, failed, and
total counts. Since Cargo starts each integration suite in a separate QEMU
process, the suite summaries are per test file; Cargo's exit status is the
aggregate result across all suites.

The suites cover VGA output, allocation, memory boundaries, user-address
validation, filesystem operations, and embedded ELF headers. The `userspace`
suite launches the C smoke tests in `tests/user_programs/` through the real ELF
loader and scheduler, captures their output, and checks completion. It uses an
in-memory filesystem for the smoke tests' files. The library and normal kernel
binary have their unit-test harnesses disabled; tests are integration targets.

## Overview

- **Process** – holds PID, name, parent/children, and owned thread IDs.
- **Thread** – owns a stack, CPU context, state, and a reference to its process.
- **Scheduler** – global run-queue, sleep queue, idle thread, and time slice (2 ticks).

## User-program tests

The finite C smoke tests live in `tests/user_programs/`. Run them with
`cargo test --test userspace`; the test kernel reports the results and exits
QEMU once all the programs finish, or fails on an error or timeout.

The `thread_b` and `thread_c` scheduler demos live in `user_programs/` and are
installed in `/bin`. Run them from the shell; they print and sleep at
different intervals until Ctrl-C.

The shell has two cases in the `userspace` suite: `sh_script` feeds it a
script file as stdin (script mode, no prompt) and checks the output of every
command, and `sh_interactive` types keys on the console (including arrow-key
escape sequences) to check the line editor and history.

`user_programs/` also contains the shared syscall header and linker script.
`build.rs` compiles the C test sources into ELF artifacts. To add a smoke test,
register its source in `build.rs` and add it to the test kernels' embedded-image
lists. The shared linker script places programs in the isolated user range.

## Process & Thread Management

- PID(0) = kernel process (auto‑created).
- Slots in vectors are recycled (`free_pids`, `free_tids`).
- Threads are created with a stack (multiple of 16 B) and an entry point.
- Fake stack frame: `ret` goes directly to entry point; interrupts enabled.

## Scheduler

### Key Fields
| Field         | Description                                |
|---------------|--------------------------------------------|
| `current`     | Currently running thread (or `None`).      |
| `runnables`   | `VecDeque<TID>` – ready threads (FIFO).    |
| `tick_count`  | Monotonically increasing tick counter.     |
| `sleep_queue` | Threads waiting for a wake‑up tick.        |
| `idle_thread` | Runs when `runnables` is empty.            |

### Behaviour
- **`tick()`** increments `tick_count`; returns `true` every `TIME_SLICE` ticks (2).
- **`schedule()`**: current thread re‑enqueued if still runnable; sleeping threads woken if time reached; idle thread selected if queue empty.
- **`sleep(ticks)`**: current thread pushed to sleep queue, set to `Sleeping`, then yields.
- **`yield_now()`** immediately calls `schedule()`.
- Preemption relies on timer IRQ calling `schedule()` on the interrupted thread’s kernel stack (no IST).

## Context Switching

`switch_context` (assembly) saves/restores callee‑saved regs + `rflags`; only `rsp` is stored in `ThreadContext`. Locks are dropped **before** the switch to avoid deadlocks.

## Thread State Machine

`Ready` → `Running` → `Blocked` | `Sleeping` | `Waiting` | `Exited` → `Zombie`.

All transitions are guarded with assertions.

## Locking Order

Always: `SCHEDULER` → `PROCESS_MANAGER`.

## Key Global Functions

| Function                | Purpose                                    |
|-------------------------|--------------------------------------------|
| `create_thread(pid,fn)` | Allocates thread, adds to run queue.       |
| `remove_thread(tid)`    | Destroys thread (must not be current).     |
| `create_idle_thread(fn)`| Sets up idle thread, registers in scheduler.|
| `schedule()`            | Pick next thread & switch.                 |
| `yield_now()`           | Voluntary yield.                           |
| `sleep(ticks)`          | Sleep for given ticks.                     |

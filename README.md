# rust-os

A modular kernel with processes, threads, and a FIFO scheduler supporting time-slice preemption and sleep queues.

## Overview

- **Process** – holds PID, name, parent/children, and owned thread IDs.
- **Thread** – owns a stack, CPU context, state, and a reference to its process.
- **Scheduler** – global run-queue, sleep queue, idle thread, and time slice (2 ticks).

## Standalone user programs

The smoke-test programs live in `user_programs/` as independent freestanding C
sources. `build.rs` compiles and links each source into an ELF file,
and the kernel embeds those build artifacts. At boot, `main` passes every
image through the regular ELF loader and starts it as its own user process.
The long-running `thread_b` and `thread_c` programs exercise scheduler sleep
and wake-up behavior after the finite smoke tests have exited.

To add another program, add its `.c` file to `user_programs/`, list its name in
`PROGRAMS` in `build.rs`, embed it in `src/main.rs`, and call
`launch_user_program`. The common linker script places programs in the
kernel's isolated user address range.

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

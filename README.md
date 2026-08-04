# rust-os

# Process & Thread Management

This module provides a cooperative multitasking kernel with processes and threads.

## Overview
- **Processes** (`Process`) group threads and hold metadata (PID, parent, name, type).
- **Threads** (`Thread`) are the schedulable units; each has its own stack and execution context.
- Scheduling is FIFO‑based; threads must explicitly yield (or be switched via timer IRQ).

## Key Data Structures
- `ProcessManager`: global singleton protected by `spin::Mutex`. Manages:
  - `processes` / `threads`: slots with `Option` for reuse.
  - `free_pids` / `free_tids`: recycled IDs.
  - `runnables`: queue of ready `TID`s.
  - `current`: currently running thread.
  - `kernel_context`: placeholder for idle switching.

## Lifecycle
- `create_process` / `create_kernel_process` / `create_user_process` allocate a `Process`.
- `create_thread` allocates a stack, initialises context, adds to process, and enqueues.
- `remove_thread` detaches and recycles TID; `destroy_process` removes all threads and recycles PID.

## Context Switching
- `switch_to(tid)` prepares old/new context pointers while holding the lock, then releases it before the assembly switch.
- Assembly saves/restores callee‑saved registers (`rbp`, `rbx`, `r12`‑`r15`) and `rsp`; only `rsp` is stored in `ThreadContext`.

## Thread States
`Ready`, `Running`, `Sleeping`, `Blocked`, `Waiting`, `Zombie`, `Exited`.

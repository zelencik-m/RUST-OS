use spin::{Mutex, MutexGuard};
use x86_64::instructions::interrupts;

/// A wrapper around spin::Mutex that disables interrupts while locked.
pub struct IrqSafeMutex<T>(Mutex<T>);

/// A guard that releases the lock and restores interrupt state when dropped.
pub struct IrqSafeMutexGuard<'a, T> {
    inner: Option<MutexGuard<'a, T>>,
    interrupts_were_enabled: bool,
}

impl<T> IrqSafeMutex<T> {
    pub const fn new(value: T) -> Self {
        IrqSafeMutex(Mutex::new(value))
    }

    pub fn lock(&self) -> IrqSafeMutexGuard<'_, T> {
        let interrupts_were_enabled = interrupts::are_enabled();

        interrupts::disable();

        // Disable interrupts before locking to prevent interrupt-handler deadlocks.
        let guard = self.0.lock();

        IrqSafeMutexGuard {
            inner: Some(guard),
            interrupts_were_enabled,
        }
    }
}

impl<T> core::ops::Deref for IrqSafeMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl<T> core::ops::DerefMut for IrqSafeMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl<T> Drop for IrqSafeMutexGuard<'_, T> {
    fn drop(&mut self) {
        // Release the lock before restoring interrupts.
        self.inner.take(); 

        if self.interrupts_were_enabled {
            interrupts::enable();
        }
    }
}

/// A lock whose waiters sleep instead of spinning, so it can be held for a
/// long time (disk I/O) with interrupts on, and the holder can be preempted.
/// It is re-entrant: the thread that holds it can lock it again (the VFS
/// does file I/O inside its own operations). It protects no data itself; it
/// serializes code.
///
/// Compared with [`IrqSafeMutex`]:
/// - Holding it keeps interrupts on, so the timer keeps running and other
///   threads get the CPU while the holder waits on the disk.
/// - Waiting for it blocks (`WaitQueue::wait_until`), which needs interrupts
///   on: never wait for it while holding an `IrqSafeMutex` (it panics), and
///   never take it in an interrupt handler.
/// - Releasing it wakes waiters, which takes SCHEDULER and PROCESS_MANAGER:
///   don't drop a guard while holding either.
///
/// The owner is the current thread's TID. Code that runs before the first
/// thread (early boot, test kernels) all counts as one owner.
pub struct SleepLock {
    state: IrqSafeMutex<SleepLockState>,
    waiters: crate::scheduler::wait_queue::WaitQueue,
}

struct SleepLockState {
    owner: Option<u64>,
    depth: usize,
}

/// Owner id for code running outside any thread (before the scheduler).
const NO_THREAD: u64 = u64::MAX;

pub struct SleepLockGuard<'a> {
    lock: &'a SleepLock,
}

impl SleepLock {
    pub const fn new() -> Self {
        SleepLock {
            state: IrqSafeMutex::new(SleepLockState { owner: None, depth: 0 }),
            waiters: crate::scheduler::wait_queue::WaitQueue::new(),
        }
    }

    fn current_owner() -> u64 {
        crate::scheduler::scheduler::SCHEDULER
            .lock()
            .get_current()
            .map_or(NO_THREAD, |tid| tid.0)
    }

    /// Take it if it's free or already ours.
    fn try_acquire(&self, owner: u64) -> bool {
        let mut state = self.state.lock();
        match state.owner {
            None => {
                state.owner = Some(owner);
                state.depth = 1;
                true
            }
            Some(current) if current == owner => {
                state.depth += 1;
                true
            }
            Some(_) => false,
        }
    }

    /// Lock, sleeping while another thread holds it.
    pub fn lock(&self) -> SleepLockGuard<'_> {
        let owner = Self::current_owner();
        if !self.try_acquire(owner) {
            self.waiters
                .wait_until(|| self.try_acquire(owner).then_some(()));
        }
        SleepLockGuard { lock: self }
    }

    /// Whether some thread holds it (for tests and assertions).
    pub fn is_locked(&self) -> bool {
        self.state.lock().owner.is_some()
    }

    /// How many threads are queued waiting for it (may include stale
    /// entries; for tests).
    pub fn waiter_count(&self) -> usize {
        self.waiters.waiter_count()
    }
}

impl Drop for SleepLockGuard<'_> {
    fn drop(&mut self) {
        let released = {
            let mut state = self.lock.state.lock();
            state.depth -= 1;
            if state.depth == 0 {
                state.owner = None;
                true
            } else {
                false
            }
        };
        if released {
            // A no-op (no other locks taken) when nobody is waiting.
            self.lock.waiters.wake_all();
        }
    }
}

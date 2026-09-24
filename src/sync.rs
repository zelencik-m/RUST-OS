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
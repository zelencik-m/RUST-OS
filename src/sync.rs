use spin::{Mutex, MutexGuard};
use x86_64::instructions::interrupts;

/// A wrapper around spin::Mutex that disables interrupts while locked.
pub struct IrqSafeMutex<T>(Mutex<T>);

/// A guard that releases the lock and restores interrupt state when dropped.
pub struct IrqSafeMutexGuard<'a, T> {
    inner: Option<MutexGuard<'a, T>>,
    interrupts_were_enabled: bool,
}

// Implement Deref and DerefMut so you can use the guard like a normal reference
impl<T> IrqSafeMutex<T> {
    pub const fn new(value: T) -> Self {
        IrqSafeMutex(Mutex::new(value))
    }

    pub fn lock(&self) -> IrqSafeMutexGuard<'_, T> {
        // 1. Check if interrupts are currently enabled BEFORE we disable them.
        let interrupts_were_enabled = interrupts::are_enabled();

        // 2. Disable interrupts.
        interrupts::disable();

        // 3. Acquire the spin lock. 
        // Because interrupts are off, no timer interrupt can interrupt us here.
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

// The magic happens here: when the guard goes out of scope, this runs.
impl<T> Drop for IrqSafeMutexGuard<'_, T> {
    fn drop(&mut self) {
        // 1. Release the spin lock by dropping the inner guard
        self.inner.take(); 

        // 2. If interrupts were enabled BEFORE we locked, re-enable them.
        if self.interrupts_were_enabled {
            interrupts::enable();
        }
    }
}
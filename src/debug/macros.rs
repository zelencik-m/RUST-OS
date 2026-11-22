//! Logging macros; see `crate::debug`.

#[doc(hidden)]
#[macro_export]
macro_rules! __log {
    ($level:expr, $($arg:tt)*) => {
        if $crate::debug::enabled($level) {
            $crate::debug::_log($level, module_path!(), format_args!($($arg)*));
        }
    };
}

/// Something is broken (the kernel can carry on).
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::__log!($crate::debug::Level::Error, $($arg)*) };
}

/// Something went wrong for a user program.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::__log!($crate::debug::Level::Warn, $($arg)*) };
}

/// Milestones (boot steps, mounts, programs started).
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::__log!($crate::debug::Level::Info, $($arg)*) };
}

/// Details for debugging a subsystem.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::__log!($crate::debug::Level::Debug, $($arg)*) };
}

/// Very frequent events (context switches, sleeps).
#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => { $crate::__log!($crate::debug::Level::Trace, $($arg)*) };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        $crate::debug::_print(format_args!($($arg)*));
    };
}


#[macro_export]
macro_rules! debugln {
    () => {
        #[cfg(debug_assertions)]
        $crate::debug::_print(format_args!("\n"));
    };

    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        $crate::debug::_print(format_args!("{}\n", format_args!($($arg)*)));
    };
}

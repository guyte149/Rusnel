use std::sync::atomic::{AtomicBool, Ordering};

pub static VERBOSE: AtomicBool = AtomicBool::new(false);

#[macro_export]
macro_rules! verbose {
    ($($arg:tt)*) => {{
        if $crate::macros::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!($($arg)*);
        }
    }};
}

pub fn set_verbose(is_verbose: bool) {
    VERBOSE.store(is_verbose, Ordering::Relaxed);
}


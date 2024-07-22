use std::sync::atomic::{AtomicBool, Ordering};

pub static VERBOSE: AtomicBool = AtomicBool::new(false);

#[macro_export]
macro_rules! verbose {
    ($($arg:tt)*) => {{
        // Use the fully qualified path for the VERBOSE static variable if necessary
        tracing::info!("hello inside macro");
        if $crate::macros::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!($($arg)*);
        }
    }};
}

// Function to set the verbosity
pub fn set_verbose(is_verbose: bool) {
    VERBOSE.store(is_verbose, Ordering::Relaxed);
}


//! Backend selection through defmt-or-log. Without a backend, diagnostics
//! compile out while their standard formatting arguments are type-checked.

macro_rules! ferriboot_log {
    ($level:ident, $($arg:tt)*) => {{
        #[cfg(any(feature = "log", feature = "defmt"))]
        ::defmt_or_log::$level!($($arg)*);
        #[cfg(not(any(feature = "log", feature = "defmt")))]
        {
            let _ = ::core::format_args!($($arg)*);
        }
    }};
}

macro_rules! error { ($($arg:tt)*) => { ferriboot_log!(error, $($arg)*) }; }
macro_rules! warn { ($($arg:tt)*) => { ferriboot_log!(warn, $($arg)*) }; }
macro_rules! info { ($($arg:tt)*) => { ferriboot_log!(info, $($arg)*) }; }
macro_rules! debug { ($($arg:tt)*) => { ferriboot_log!(debug, $($arg)*) }; }

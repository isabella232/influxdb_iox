#![recursion_limit = "512"] // required for print_cpu
#![deny(rustdoc::broken_intra_doc_links, rustdoc::bare_urls, rust_2018_idioms)]
#![warn(
    missing_debug_implementations,
    clippy::explicit_iter_loop,
    clippy::use_self,
    clippy::clone_on_ref_ptr,
    clippy::future_not_send
)]

use dotenv::dotenv;
use once_cell::sync::Lazy;
use std::str::FromStr;
use tokio::runtime::Runtime;

pub mod commands {
    pub mod catalog;
    pub mod database;
    pub mod debug;
    pub mod operations;
    pub mod router;
    pub mod run;
    pub mod server;
    pub mod server_remote;
    pub mod sql;
    pub mod storage;
    pub mod tracing;
}

pub mod influxdb_ioxd;

#[derive(Debug)]
pub enum ReturnCode {
    Failure = 1,
}

pub static VERSION_STRING: Lazy<String> = Lazy::new(|| {
    format!(
        "{}, revision {}",
        option_env!("CARGO_PKG_VERSION").unwrap_or("UNKNOWN"),
        option_env!("GIT_HASH").unwrap_or("UNKNOWN")
    )
});

/// A comfy_table style that uses single ASCII lines for all borders with plusses at intersections.
///
/// Example:
///
/// ```text
/// +------+--------------------------------------+
/// | Name | UUID                                 |
/// +------+--------------------------------------+
/// | bar  | ccc2b8bc-f25d-4341-9b64-b9cfe50d26de |
/// | foo  | 3317ff2b-bbab-43ae-8c63-f0e9ea2f3bdb |
/// +------+--------------------------------------+
/// ```
const TABLE_STYLE_SINGLE_LINE_BORDERS: &str = "||--+-++|    ++++++";

#[cfg(all(
    feature = "heappy",
    feature = "jemalloc_replacing_malloc",
    not(feature = "clippy")
))]
compile_error!("heappy and jemalloc_replacing_malloc features are mutually exclusive");

/// Creates the tokio runtime for executing IOx
///
/// if nthreads is none, uses the default scheduler
/// otherwise, creates a scheduler with the number of threads
pub fn get_runtime(num_threads: Option<usize>) -> Result<Runtime, std::io::Error> {
    // NOTE: no log macros will work here!
    //
    // That means use eprintln!() instead of error!() and so on. The log emitter
    // requires a running tokio runtime and is initialised after this function.

    use tokio::runtime::Builder;
    let kind = std::io::ErrorKind::Other;
    match num_threads {
        None => Runtime::new(),
        Some(num_threads) => {
            println!(
                "Setting number of threads to '{}' per command line request",
                num_threads
            );

            match num_threads {
                0 => {
                    let msg = format!(
                        "Invalid num-threads: '{}' must be greater than zero",
                        num_threads
                    );
                    Err(std::io::Error::new(kind, msg))
                }
                1 => Builder::new_current_thread().enable_all().build(),
                _ => Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(num_threads)
                    .build(),
            }
        }
    }
}

/// Source the .env file before initialising the Config struct - this sets
/// any envs in the file, which the Config struct then uses.
///
/// Precedence is given to existing env variables.
pub fn load_dotenv() {
    match dotenv() {
        Ok(_) => {}
        Err(dotenv::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            // Ignore this - a missing env file is not an error, defaults will
            // be applied when initialising the Config struct.
        }
        Err(e) => {
            eprintln!("FATAL Error loading config from: {}", e);
            eprintln!("Aborting");
            std::process::exit(1);
        }
    };
}

// Based on ideas from
// https://github.com/servo/servo/blob/f03ddf6c6c6e94e799ab2a3a89660aea4a01da6f/ports/servo/main.rs#L58-L79
pub fn install_crash_handler() {
    unsafe {
        set_signal_handler(libc::SIGSEGV, signal_handler); // handle segfaults
        set_signal_handler(libc::SIGILL, signal_handler); // handle stack overflow and unsupported CPUs
        set_signal_handler(libc::SIGBUS, signal_handler); // handle invalid memory access
    }
}

unsafe extern "C" fn signal_handler(sig: i32) {
    use backtrace::Backtrace;
    use std::process::abort;
    let name = std::thread::current()
        .name()
        .map(|n| format!(" for thread \"{}\"", n))
        .unwrap_or_else(|| "".to_owned());
    eprintln!(
        "Signal {}, Stack trace{}\n{:?}",
        sig,
        name,
        Backtrace::new()
    );
    abort();
}

// based on https://github.com/adjivas/sig/blob/master/src/lib.rs#L34-L52
unsafe fn set_signal_handler(signal: libc::c_int, handler: unsafe extern "C" fn(libc::c_int)) {
    use libc::{sigaction, sigfillset, sighandler_t};
    let mut sigset = std::mem::zeroed();

    // Block all signals during the handler. This is the expected behavior, but
    // it's not guaranteed by `signal()`.
    if sigfillset(&mut sigset) != -1 {
        // Done because sigaction has private members.
        // This is safe because sa_restorer and sa_handlers are pointers that
        // might be null (that is, zero).
        let mut action: sigaction = std::mem::zeroed();

        // action.sa_flags = 0;
        action.sa_mask = sigset;
        action.sa_sigaction = handler as sighandler_t;

        sigaction(signal, &action, std::ptr::null_mut());
    }
}

/// A ':' separated key value pair
#[derive(Debug, Clone)]
pub struct KeyValue<K, V> {
    pub key: K,
    pub value: V,
}

impl<K, V> std::str::FromStr for KeyValue<K, V>
where
    K: FromStr,
    V: FromStr,
    K::Err: std::fmt::Display,
    V::Err: std::fmt::Display,
{
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use itertools::Itertools;
        match s.splitn(2, ':').collect_tuple() {
            Some((key, value)) => {
                let key = K::from_str(key).map_err(|e| e.to_string())?;
                let value = V::from_str(value).map_err(|e| e.to_string())?;
                Ok(Self { key, value })
            }
            None => Err(format!(
                "Invalid key value pair - expected 'KEY:VALUE' got '{}'",
                s
            )),
        }
    }
}

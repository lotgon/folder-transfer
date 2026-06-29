//! folder-transfer core library.
//!
//! The same code powers the `ft` CLI (src/main.rs) and a C-ABI shared library
//! (`ft.dll` / `libft.so`) for embedding in .NET, C/C++, etc. — see the `ffi`
//! module. The high-level entry points are [`client::run`] (download) and
//! [`server::run_serve_single`] / [`server::run_serve_parallel`] (serve).

pub mod client;
pub mod compress;
pub mod config;
pub mod ffi;
pub mod firewall;
pub mod ignore;
pub mod mtime;
pub mod paths;
pub mod progress;
pub mod server;
pub mod tls;
pub mod token;
pub mod wire;

/// Shared error type for fallible operations.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Default TCP port (matches `ft-server.ps1`).
pub const DEFAULT_PORT: u16 = 8722;

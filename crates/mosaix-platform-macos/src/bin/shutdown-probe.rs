//! A single-threaded process that waits for a shutdown signal.
//!
//! It exists for `tests/shutdown_signal.rs`. The agent's hang could only
//! be reproduced in a process where the thread parked on the receiver is
//! also the thread the signal is delivered to, which a libtest worker
//! thread is not -- so the regression test needs a real, minimal binary
//! rather than a unit test.
fn main() {
    let shutdown =
        mosaix_platform_macos::register_shutdown_signal().expect("registers in a fresh process");
    println!("READY");
    let _ = shutdown.recv();
    println!("STOPPED");
}

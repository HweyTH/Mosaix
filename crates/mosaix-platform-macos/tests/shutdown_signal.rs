//! The agent must stop on SIGTERM, because that is the path that gets a
//! chance to restore parked windows; SIGKILL does not (ADR 0023).
#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_sigterm_stops_the_process_gracefully() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_shutdown-probe"))
        .stdout(Stdio::piped())
        .spawn()
        .expect("the probe binary starts");

    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut line = String::new();
    stdout
        .read_line(&mut line)
        .expect("the probe reports READY");
    assert_eq!(line.trim(), "READY");

    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the child is waitable") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the process ignored SIGTERM and had to be killed");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert!(
        status.success(),
        "a graceful stop exits cleanly, not on a signal: {status:?}"
    );
    line.clear();
    stdout
        .read_line(&mut line)
        .expect("the probe reports STOPPED");
    assert_eq!(
        line.trim(),
        "STOPPED",
        "the shutdown receiver must actually wake"
    );
}

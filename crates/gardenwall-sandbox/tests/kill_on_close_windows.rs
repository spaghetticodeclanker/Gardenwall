//! A kill-on-close job ends its processes when its handle closes (which
//! Windows also does when the owner is killed), so an unsandboxed helper
//! never outlives the tool that started it.
#![cfg(windows)]

use std::process::Command;
use std::time::{Duration, Instant};

use gardenwall_sandbox::KillOnCloseJob;

#[test]
fn closing_the_job_ends_its_process() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sandbox-probe"))
        .args(["sleep", "60"])
        .spawn()
        .unwrap();
    let job = KillOnCloseJob::new().unwrap();
    job.assign(child.id()).unwrap();
    assert!(child.try_wait().unwrap().is_none(), "still running");
    drop(job);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "the process outlived its job");
        std::thread::sleep(Duration::from_millis(50));
    }
}

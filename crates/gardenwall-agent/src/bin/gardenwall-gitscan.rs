//! Gardenwall's pre-commit secret check, staged as `pre-commit` in a
//! read-only hooks folder that git uses for the agent's commands only (see
//! `gardenwall_agent::commit_check`). Runs inside the session's sandbox.

//!
//! `gardenwall-gitscan --scan <from> <to>`: the core's own scan of what
//! `<to>` adds over `<from>`, printed as one summary line (see
//! `commit_check::run_scan`).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.as_slice() {
        [flag, from, to] if flag == "--scan" => gardenwall_agent::commit_check::run_scan(from, to),
        _ => gardenwall_agent::commit_check::run_hook(),
    };
    std::process::exit(code);
}

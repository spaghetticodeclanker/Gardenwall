//! A session's shell that can be replaced while the conversation goes on
//! (ADR-024 §3 A1): the sandbox's folders are fixed when it starts, so
//! adding a toolchain means a new sandbox. The session and the app keep one
//! [`SessionShell`]; a restart swaps the sandbox inside it.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::{Aftermath, BoxFuture, Shell, ShellOutput};

/// Something dropped right after the shell it came with (the session's
/// scratch folder, which must outlive the sandbox that uses it).
pub type Keep = Box<dyn Send + Sync>;

/// Why a restart was refused.
pub const WAITING: &str = "A command's changes or the programs it left running are waiting for the user's decision. The sandbox can restart once that card is answered.";

const CHANGES: u8 = 1;
const PROGRAMS: u8 = 2;

pub struct SessionShell {
    inner: Mutex<(Arc<dyn Shell>, Option<Keep>)>,
    /// A command left something for the user to decide that hasn't been
    /// settled yet: held-back changes (`CHANGES`), programs left running
    /// (`PROGRAMS`).
    unsettled: AtomicU8,
}

impl SessionShell {
    pub fn new(shell: Arc<dyn Shell>, keep: Option<Keep>) -> Self {
        Self {
            inner: Mutex::new((shell, keep)),
            unsettled: AtomicU8::new(0),
        }
    }

    /// The sandbox in use now.
    pub fn current(&self) -> Arc<dyn Shell> {
        match self.inner.lock() {
            Ok(g) => g.0.clone(),
            Err(e) => e.into_inner().0.clone(),
        }
    }

    /// A command card is waiting (a restart is refused until it's answered).
    pub fn waiting(&self) -> bool {
        self.unsettled.load(Ordering::SeqCst) != 0
    }

    /// Use `shell` from now on. The old sandbox's programs the user let keep
    /// running are stopped, then the old sandbox and what came with it
    /// (`keep`) are dropped, in that order. Refused while a command card
    /// waits.
    pub fn replace(&self, shell: Arc<dyn Shell>, keep: Option<Keep>) -> Result<(), String> {
        if self.waiting() {
            return Err(WAITING.into());
        }
        let (old, old_keep) = {
            let mut g = self.inner.lock().map_err(|_| "shell state unavailable")?;
            std::mem::replace(&mut *g, (shell, keep))
        };
        old.stop_background();
        drop(old);
        drop(old_keep);
        Ok(())
    }
}

impl Shell for SessionShell {
    fn name(&self) -> &'static str {
        self.current().name()
    }
    fn tool_name(&self) -> &'static str {
        self.current().tool_name()
    }
    fn snippet(&self) -> &'static str {
        self.current().snippet()
    }
    fn guidelines(&self) -> &'static [&'static str] {
        self.current().guidelines()
    }
    fn drives(&self) -> Vec<(char, std::path::PathBuf)> {
        self.current().drives()
    }
    fn run<'a>(
        &'a self,
        command: &'a str,
        timeout: Option<Duration>,
    ) -> BoxFuture<'a, Result<ShellOutput, String>> {
        let shell = self.current();
        Box::pin(async move { shell.run(command, timeout).await })
    }
    fn program_command(&self, program: &std::path::Path, args: &[&str]) -> Option<String> {
        self.current().program_command(program, args)
    }
    fn aftermath(&self) -> Option<Aftermath> {
        let a = self.current().aftermath();
        if let Some(a) = &a {
            let bits = if a.changes.is_empty() { 0 } else { CHANGES }
                | if a.programs.is_empty() { 0 } else { PROGRAMS };
            self.unsettled.fetch_or(bits, Ordering::SeqCst);
        }
        a
    }
    fn settle_changes(&self, keep: bool) -> Vec<String> {
        let left = self.current().settle_changes(keep);
        self.unsettled.fetch_and(!CHANGES, Ordering::SeqCst);
        left
    }
    fn settle_programs(&self, keep: bool) {
        self.current().settle_programs(keep);
        self.unsettled.fetch_and(!PROGRAMS, Ordering::SeqCst);
    }
    fn background(&self) -> Vec<String> {
        self.current().background()
    }
    fn stop_background(&self) {
        self.current().stop_background()
    }
    fn kill_now(&self) {
        self.current().kill_now()
    }
    fn hidden_files(&self) -> Vec<String> {
        self.current().hidden_files()
    }
    fn protect_new(&self) {
        self.current().protect_new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    struct Fake {
        name: &'static str,
        stopped: Arc<AtomicUsize>,
        leaves: bool,
    }

    impl Shell for Fake {
        fn name(&self) -> &'static str {
            self.name
        }
        fn run<'a>(
            &'a self,
            _command: &'a str,
            _timeout: Option<Duration>,
        ) -> BoxFuture<'a, Result<ShellOutput, String>> {
            let name = self.name;
            Box::pin(async move {
                Ok(ShellOutput {
                    output: name.into(),
                    exit_code: Some(0),
                    timed_out: false,
                })
            })
        }
        fn aftermath(&self) -> Option<Aftermath> {
            self.leaves.then(|| Aftermath {
                programs: vec!["server.exe".into()],
                ..Default::default()
            })
        }
        fn stop_background(&self) {
            self.stopped.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_restart_swaps_the_sandbox_and_drops_the_old_one() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let s = SessionShell::new(
            Arc::new(Fake {
                name: "old",
                stopped: stopped.clone(),
                leaves: true,
            }),
            Some(Box::new(Flag(dropped.clone()))),
        );
        assert_eq!(s.run("x", None).await.unwrap().output, "old");
        // A card waits: refused, nothing changes.
        assert!(s.aftermath().is_some());
        let new = || {
            Arc::new(Fake {
                name: "new",
                stopped: Arc::new(AtomicUsize::new(0)),
                leaves: false,
            })
        };
        assert_eq!(s.replace(new(), None).unwrap_err(), WAITING);
        assert!(!dropped.load(Ordering::SeqCst));
        s.settle_programs(true);
        s.replace(new(), None).unwrap();
        assert_eq!(s.run("x", None).await.unwrap().output, "new");
        assert_eq!(s.name(), "new");
        assert_eq!(stopped.load(Ordering::SeqCst), 1, "kept programs stop");
        assert!(dropped.load(Ordering::SeqCst), "the old scratch goes");
    }
}

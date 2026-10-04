//! Test double for the host runner: records command lines and replays canned
//! responses, plus a fake `/host` tree with real-looking devices.

use async_trait::async_trait;
use naslos_agent::zfs::runner::{RunError, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

enum Response {
    Ok(String),
    Err(String),
}

pub struct FakeRunner {
    calls: Arc<Mutex<Vec<String>>>,
    responses: Mutex<Vec<Response>>,
    idx: AtomicUsize,
}

impl FakeRunner {
    pub fn new(responses: &[&str]) -> Self {
        let responses = responses
            .iter()
            .map(|r| match r.strip_prefix("ERR:") {
                Some(msg) => Response::Err(msg.to_string()),
                None => Response::Ok(r.to_string()),
            })
            .collect();
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            responses: Mutex::new(responses),
            idx: AtomicUsize::new(0),
        }
    }

    /// A shared handle to the recorded command lines.
    #[allow(dead_code)]
    pub fn calls(&self) -> Arc<Mutex<Vec<String>>> {
        self.calls.clone()
    }

    fn respond(&self) -> Result<String, RunError> {
        let i = self.idx.fetch_add(1, Ordering::SeqCst);
        let guard = self.responses.lock().unwrap();
        match guard.get(i) {
            Some(Response::Ok(out)) => Ok(out.clone()),
            Some(Response::Err(msg)) => Err(RunError {
                output: msg.clone(),
                message: "exit status 1".to_string(),
            }),
            None => Ok(String::new()),
        }
    }
}

#[async_trait]
impl Runner for FakeRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<String, RunError> {
        let mut line = bin.to_string();
        for a in args {
            line.push(' ');
            line.push_str(a);
        }
        self.calls.lock().unwrap().push(line);
        self.respond()
    }
}

/// A fake `/host` tree containing `/dev/{sdb,sdc,sdd,sde}`.
pub struct FakeHost {
    _dir: tempfile::TempDir,
    pub root: std::path::PathBuf,
}

impl FakeHost {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("dev")).unwrap();
        for name in ["sdb", "sdc", "sdd", "sde"] {
            std::fs::write(root.join("dev").join(name), b"").unwrap();
        }
        Self { _dir: dir, root }
    }
}

/// Whether any recorded call contains `substr`.
#[allow(dead_code)]
pub fn ran_command(calls: &Arc<Mutex<Vec<String>>>, substr: &str) -> bool {
    calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.contains(substr))
}

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vis-rs"))
}

#[test]
fn batch_edits_binary_stdin_and_reports_errors() {
    let mut child = binary()
        .args(["--command", ",x/♪/ c/♫/"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"\0\xff\xe2\x99\xaa\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"\0\xff\xe2\x99\xab\n");

    let result = binary()
        .args(["--command", ",{c/one/"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!result.stderr.is_empty());
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vis-batch-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn write_commits_successful_scripts_only() {
    let dir = Directory::new();
    let file = dir.0.join("input");
    let script = dir.0.join("script");
    std::fs::write(&file, b"Emacs vi\n").unwrap();
    std::fs::write(&script, ",x/Emacs/ c/vis/").unwrap();
    let result = binary()
        .args(["--write", "--atomic", "--batch"])
        .arg(&script)
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty());
    assert_eq!(std::fs::read(&file).unwrap(), b"vis vi\n");
    let result = binary()
        .args(["--write", "--command", ",{c/a/ c/b/}"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&file).unwrap(), b"vis vi\n");
}

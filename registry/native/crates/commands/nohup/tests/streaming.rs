use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

fn spawn_stdout_reader(stdout: ChildStdout) -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = stdout;
        let mut buf = [0_u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Some(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(error) => panic!("failed to read nohup stdout: {error}"),
            }
        }
        let _ = tx.send(None);
    });
    rx
}

fn spawn_nohup(script: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_nohup"))
        .args(["sh", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn nohup test command")
}

#[test]
fn nohup_streams_stdout_before_child_exit() {
    let script = r#"
i=0
while [ "$i" -lt 128 ]; do
  head -c 8192 /dev/zero | tr '\000' x
  i=$((i + 1))
  sleep 0.015625
done
"#;

    let mut child = spawn_nohup(script);
    let stdout = child.stdout.take().expect("missing nohup stdout");
    let rx = spawn_stdout_reader(stdout);

    let first_chunk = rx
        .recv_timeout(Duration::from_millis(750))
        .expect("expected nohup to stream stdout before exit")
        .expect("nohup stdout closed before first chunk");
    assert!(!first_chunk.is_empty());
    assert_eq!(
        child.try_wait().expect("failed to poll nohup child"),
        None,
        "nohup child exited before the first chunk was observed"
    );

    let status = child.wait().expect("failed to wait for nohup child");
    assert!(status.success(), "nohup exited with {status:?}");

    let mut total = first_chunk.len();
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Some(chunk)) => total += chunk.len(),
            Ok(None) => break,
            Err(error) => panic!("timed out draining nohup stdout: {error}"),
        }
    }

    assert_eq!(total, 128 * 8192);
}

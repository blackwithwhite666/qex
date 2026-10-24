//! One Linux regression in its own test process so subreaper ownership cannot
//! affect the other integration tests or adopt their children.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    end: Instant,
    daemon: Option<Child>,
}

impl Fixture {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_qex"));
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("USER", unsafe { libc::geteuid() }.to_string())
            .env("XDG_CONFIG_HOME", self.root.join("cfg"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("QEX_PEERS_DIR", self.root.join("peers"))
            .env("QEX_IDLE_EXIT_SECS", "2")
            .env("CODEX_THREAD_ID", "01900000-0000-7000-8000-000000000010")
            .stdin(Stdio::null())
            .process_group(0);
        command
    }

    fn phase_end(&self) -> Instant {
        self.end - Duration::from_secs(5)
    }

    fn call(&self, args: &[&str]) -> String {
        let output = self.root.join("call.out");
        let errors = self.root.join("call.err");
        let mut command = self.command(args);
        command
            .stdout(std::fs::File::create(&output).unwrap())
            .stderr(std::fs::File::create(&errors).unwrap());
        let mut child = command.spawn().unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= self.phase_end() {
                unsafe {
                    libc::killpg(child.id() as i32, libc::SIGKILL);
                }
                child.wait().unwrap();
                panic!("{args:?} exceeded the scenario deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            status.success(),
            "{args:?}: {}",
            std::fs::read_to_string(errors).unwrap()
        );
        std::fs::read_to_string(output).unwrap()
    }

    fn children() -> Vec<i32> {
        let mut children = std::collections::BTreeSet::new();
        // Cargo's test runs on a worker thread. Include every task's children,
        // including orphans adopted by the process's main thread.
        for task in std::fs::read_dir("/proc/self/task").unwrap() {
            match std::fs::read_to_string(task.unwrap().path().join("children")) {
                Ok(pids) => children.extend(
                    pids.split_whitespace()
                        .map(|pid| pid.parse::<i32>().unwrap()),
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => panic!("cannot inspect owned test children: {e}"),
            }
        }
        children.into_iter().collect()
    }

    fn cleanup(&mut self) {
        // This is a separate, sole-test subreaper process. Its direct/adopted
        // children all belong to this fixture. Signal before reaping, so no
        // historical/reused PID is ever signaled. Repeat only for adoption
        // after killing an owned parent on an assertion-failure path.
        loop {
            for pid in Self::children() {
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
            loop {
                let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
                if pid <= 0 {
                    break;
                }
            }
            if Self::children().is_empty() {
                break;
            }
            assert!(
                Instant::now() < self.end,
                "owned test children did not join"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.daemon.take();
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("fixture removal failed: {e}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Always join owned processes, including when the original assertion
        // identifies the unfixed defect. Keep cleanup failures visible.
        if let Err(error) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.cleanup()))
        {
            if std::thread::panicking() {
                eprintln!("notification shutdown fixture cleanup failed: {error:?}");
            } else {
                std::panic::resume_unwind(error);
            }
        }
    }
}

#[test]
fn idle_exit_finishes_owned_notification_timeout_and_cleanup() {
    let end = Instant::now() + Duration::from_secs(45);
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let root = std::env::temp_dir().join(format!(
        "qn{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir(&root).unwrap();
    let mut fixture = Fixture {
        root,
        end,
        daemon: None,
    };
    std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["cfg", "state", "run", "peers"] {
        std::fs::create_dir(fixture.root.join(name)).unwrap();
    }
    std::fs::set_permissions(
        fixture.root.join("peers"),
        std::fs::Permissions::from_mode(0o1777),
    )
    .unwrap();
    std::fs::write(fixture.root.join("cfg/qex.toml"), "[peers]\nenabled=false\n[system]\nreserve_mem=\"0\"\nmax_pressure=100\n[update]\ncheck=\"never\"\n").unwrap();
    let log = fixture.root.join("daemon.log");
    let output = std::fs::File::create(&log).unwrap();
    fixture.daemon = Some(
        fixture
            .command(&["daemon", "--foreground"])
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap(),
    );
    while !std::fs::read_to_string(&log)
        .unwrap()
        .contains("the coordinator started")
    {
        assert!(
            Instant::now() < fixture.phase_end(),
            "coordinator did not become ready"
        );
        assert!(fixture
            .daemon
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
    let socket = fixture.root.join("s");
    let _listener = UnixListener::bind(&socket).unwrap();
    let client = fixture.root.join("client");
    let client_pid = fixture.root.join("client.pid");
    std::fs::write(
        &client,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 60\n",
            client_pid.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    fixture.call(&["pause", "queue"]);
    let job = fixture.call(&[
        "submit",
        "--cpu",
        "1",
        "--mem",
        "1MB",
        "--env-capture",
        "none",
        "--notify-codex",
        socket.to_str().unwrap(),
        "--codex-client",
        client.to_str().unwrap(),
        "--",
        "/bin/true",
    ]);
    let job = job.trim();
    fixture.call(&["cancel", job]);
    let pid: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&client_pid)
            .ok()
            .and_then(|pid| pid.trim().parse().ok())
        {
            break pid;
        }
        assert!(
            Instant::now() < fixture.phase_end(),
            "fake client did not become ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let observed = Instant::now();
    // A single purposeful observation proves the callback owns no scheduler
    // lock while waiting. No command polls the model or drives shutdown.
    let info_started = Instant::now();
    fixture.call(&["info", "--no-start", "--json"]);
    assert!(info_started.elapsed() < Duration::from_secs(2));
    let events_path = fixture.root.join("events.jsonl");
    let mut events = fixture
        .command(&["events", "--json", "--since", "now", "--timeout", "38s"])
        .stdout(std::fs::File::create(&events_path).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    while std::fs::read(&events_path).unwrap().is_empty() {
        assert!(
            Instant::now() < fixture.phase_end(),
            "event reader did not attach"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = loop {
        if let Some(status) = fixture.daemon.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < fixture.phase_end(),
            "coordinator did not exit normally after bounded cleanup"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    assert!(
        observed.elapsed() <= Duration::from_secs(34),
        "callback exceeded its timeout plus idle-exit allowance"
    );
    let outcome = fixture
        .root
        .join("state/qex/jobs")
        .join(job)
        .join("notify-codex.outcome.json");
    let outcome: serde_json::Value = serde_json::from_slice(
        &std::fs::read(outcome).expect("normal exit must retain notification outcome"),
    )
    .unwrap();
    assert_eq!(outcome["view"]["state"], "unknown");
    assert_eq!(outcome["job"], job);
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "fake client survived coordinator exit"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(std::fs::read_to_string(log)
        .unwrap()
        .contains("the coordinator stopped"));
    let events_status = loop {
        if let Some(status) = events.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < fixture.phase_end(),
            "passive event reader prevented idle/bye shutdown"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(events_status.success());
    assert!(std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|event| event["event"] == "bye")
        }));
    fixture.cleanup();
    drop(fixture);
    assert!(Instant::now() < end);
}

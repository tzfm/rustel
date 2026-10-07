//! Terminal-only CLI contracts: consent, interrupt handling and colour.
use super::{rustel, scratch, wait_for_output};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct Terminal {
    master: File,
    slave: File,
}

impl Terminal {
    fn new() -> Self {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty writes two fresh owned descriptors to valid pointers;
        // null optional arguments request the system's default terminal setup.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: the successful call transferred both descriptors to us.
        let terminal = unsafe {
            Self {
                master: File::from_raw_fd(master),
                slave: File::from_raw_fd(slave),
            }
        };
        // Nonblocking reads allow a deadline even if the child emits nothing.
        // SAFETY: master is a live descriptor and these commands take integers.
        unsafe {
            let flags = libc::fcntl(terminal.master.as_raw_fd(), libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(
                libc::fcntl(
                    terminal.master.as_raw_fd(),
                    libc::F_SETFL,
                    flags | libc::O_NONBLOCK
                ),
                0
            );
        }
        terminal
    }

    fn stream(&self) -> Stdio {
        self.slave.try_clone().unwrap().into()
    }

    fn read_until(
        &mut self,
        child: &mut Child,
        ready: impl Fn(&str, &mut Child) -> bool,
    ) -> String {
        let mut screen = String::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let mut bytes = [0u8; 16384];
            match self.master.read(&mut bytes) {
                Ok(n) => screen.push_str(&String::from_utf8_lossy(&bytes[..n])),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("terminal read: {error}"),
            }
            if ready(&screen, child) {
                return screen;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("terminal command timed out: {screen}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn terminal_cache_confirmation_accepts_only_yes_and_can_be_interrupted() {
    for command in [vec!["samples", "clear"], vec!["clear-score-cache"]] {
        for response in ["no", "yes", "interrupt"] {
            let base = scratch(&format!("terminal-consent-{}-{response}", command[0]));
            let entry = base.join("score").join("response.wav");
            std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
            std::fs::write(&entry, b"cached score sample").unwrap();
            let mut terminal = Terminal::new();
            let mut child = rustel()
                .args(&command)
                .env("RUSTEL_SAMPLE_CACHE", &base)
                .stdin(terminal.stream())
                .stderr(terminal.stream())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let prompt = terminal.read_until(&mut child, |screen, _| screen.contains("Type yes"));
            assert!(prompt.contains(base.to_str().unwrap()));
            assert!(entry.exists(), "deleted before consent");
            if command[0] == "clear-score-cache" {
                serde_json::from_str::<serde_json::Value>(prompt.trim()).unwrap();
            }
            if response == "interrupt" {
                // SAFETY: signal only the child process owned by this test.
                assert_eq!(
                    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
                    0
                );
            } else {
                writeln!(terminal.master, "{response}").unwrap();
            }
            let output = wait_for_output(child, &command);
            let expected = match response {
                "yes" => 0,
                "interrupt" => 130,
                _ => 1,
            };
            assert_eq!(output.status.code(), Some(expected), "{output:?}");
            assert_eq!(entry.exists(), response != "yes");
            std::fs::remove_dir_all(base).unwrap();
        }
    }
}

#[test]
fn terminal_help_honours_empty_no_color_and_display_flags() {
    for (no_color, flags, coloured) in [
        (None, vec![], true),
        (Some(""), vec![], true),
        (Some("1"), vec![], false),
        (Some(""), vec!["--no-color"], false),
        (Some(""), vec!["--plain"], false),
    ] {
        let mut terminal = Terminal::new();
        let mut command = rustel();
        command
            .args(flags)
            .arg("--help")
            .env("TERM", "xterm-256color")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("CLICOLOR");
        if let Some(value) = no_color {
            command.env("NO_COLOR", value);
        } else {
            command.env_remove("NO_COLOR");
        }
        let mut child = command
            .stdout(terminal.stream())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let screen = terminal.read_until(&mut child, |screen, child| {
            screen.contains("https://github.com/tzfm/rustel/issues")
                && child.try_wait().unwrap().is_some()
        });
        let output = wait_for_output(child, &["--help"]);
        assert!(output.status.success());
        assert_eq!(screen.contains('\x1b'), coloured, "{screen}");
    }
}

#[test]
fn terminal_stdin_gets_a_watch_code_pipe_hint() {
    let terminal = Terminal::new();
    let child = rustel()
        .arg("watch-code")
        .stdin(terminal.stream())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let output = wait_for_output(child, &["watch-code"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("expects piped score events"));
}

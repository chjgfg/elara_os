#![forbid(unsafe_code)]

#[cfg(windows)]
mod windows_console {
    use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
    use std::io::{Read, Write};
    use std::path::Path;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use std::{assert, env, format, fs, println};

    struct OwnedChild(Box<dyn Child + Send + Sync>);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
        }
    }

    fn wait_for(
        receiver: &Receiver<Vec<u8>>,
        captured: &mut Vec<u8>,
        writer: &mut dyn Write,
        cursor_answered: &mut bool,
        needle: &str,
        since: usize,
    ) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let text = String::from_utf8_lossy(captured);
            // ConPTY asks the terminal emulator for the initial cursor position.
            if !*cursor_answered && text.contains("\x1b[6n") {
                writer.write_all(b"\x1b[1;1R").unwrap();
                writer.flush().unwrap();
                *cursor_answered = true;
            }
            if String::from_utf8_lossy(&captured[since..]).contains(needle) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = receiver.recv_timeout(remaining).unwrap_or_else(|error| {
                std::panic!("ConPTY did not produce {needle:?}: {error}; transcript={text:?}")
            });
            captured.extend_from_slice(&chunk);
        }
    }

    #[test]
    fn native_password_input_is_hidden_and_normal_echo_is_restored() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"));
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture = project
            .join("target/test-data")
            .join(format!("native-tty-{nonce}"));
        fs::create_dir_all(&fixture).unwrap();
        let root = fixture.join("os-root");
        let configuration = fixture.join("runtime.env");
        fs::write(
            &configuration,
            format!("ELARA_DATA_ROOT={}\n", root.display()).as_bytes(),
        )
        .unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = [0u8; 4096];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 || sender.send(bytes[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_elara-host"));
        command.arg(&configuration);
        command.cwd(project);
        command.env("TERM", "xterm-256color");
        let mut child = OwnedChild(pair.slave.spawn_command(command).unwrap());
        drop(pair.slave);
        let mut captured = Vec::new();
        let mut cursor_answered = false;
        wait_for(
            &receiver,
            &mut captured,
            &mut *writer,
            &mut cursor_answered,
            "New password for root:",
            0,
        );
        // Model a human typing after the prompt; this is not a fast-paste test.
        thread::sleep(Duration::from_millis(100));
        let checkpoint = captured.len();
        writer.write_all(b"NativeRootSecret_9284\r").unwrap();
        writer.flush().unwrap();
        wait_for(
            &receiver,
            &mut captured,
            &mut *writer,
            &mut cursor_answered,
            "Retype new password:",
            checkpoint,
        );
        thread::sleep(Duration::from_millis(100));
        let checkpoint = captured.len();
        writer.write_all(b"NativeRootSecret_9284\r").unwrap();
        writer.flush().unwrap();
        wait_for(
            &receiver,
            &mut captured,
            &mut *writer,
            &mut cursor_answered,
            "root@elara:/>",
            checkpoint,
        );
        assert!(!String::from_utf8_lossy(&captured).contains("NativeRootSecret_9284"));

        // Before Enter, the command can only appear through restored input echo.
        let checkpoint = captured.len();
        writer.write_all(b"echo TERM_ECHO_CONTROL").unwrap();
        writer.flush().unwrap();
        wait_for(
            &receiver,
            &mut captured,
            &mut *writer,
            &mut cursor_answered,
            "echo TERM_ECHO_CONTROL",
            checkpoint,
        );
        let checkpoint = captured.len();
        writer.write_all(b"\r").unwrap();
        writer.flush().unwrap();
        wait_for(
            &receiver,
            &mut captured,
            &mut *writer,
            &mut cursor_answered,
            "root@elara:/>",
            checkpoint,
        );
        let checkpoint = captured.len();
        writer.write_all(b"exit\r").unwrap();
        writer.flush().unwrap();
        wait_for(
            &receiver,
            &mut captured,
            &mut *writer,
            &mut cursor_answered,
            "Bye.",
            checkpoint,
        );
        assert!(child.0.wait().unwrap().success());
        let text = String::from_utf8_lossy(&captured);
        assert!(!text.contains("NativeRootSecret_9284"));
        assert!(text.contains("Password set for root."));
        assert!(
            !fs::read_to_string(root.join("etc/shadow"))
                .unwrap()
                .contains("NativeRootSecret_9284")
        );
        assert!(
            !fs::read_to_string(root.join("var/log/elara.log"))
                .unwrap()
                .contains("NativeRootSecret_9284")
        );
        println!("PASS: native ConPTY password hidden; ordinary input echo restored; exit 0; no plaintext in shadow/log.");
    }
}

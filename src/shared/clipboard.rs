use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

use log::error;

pub fn copy_to_clipboard(text: String) {
    // Spawn a thread so the UI is not blocked
    thread::spawn(move || {
        let child = Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        match child {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }

                let output = child.wait_with_output();
                if let Ok(out) = output {
                    if !out.status.success() {
                        error!(
                            "xclip failed: {:?}",
                            String::from_utf8_lossy(&out.stderr)
                        );
                    }
                } else if let Err(e) = output {
                    error!("Failed to wait xclip: {}", e);
                }
            }
            Err(e) => error!("Failed to spawn xclip: {}", e),
        }
    });
}

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_STDOUT: usize = 256 * 1024;
const MAX_STDERR: usize = 16 * 1024;

pub(super) fn run(args: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = Command::new("gh");
    command.args(args);
    capture(&mut command, TIMEOUT)
}

fn capture(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PAGER", "cat")
        .env("PAGER", "cat")
        .env("NO_COLOR", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "gh is not installed or not on PATH".to_owned()
        } else {
            format!("could not start gh: {error}")
        }
    })?;
    let (sender, receiver) = mpsc::channel();
    for (index, pipe, limit) in [
        (
            0,
            Box::new(child.stdout.take().expect("piped stdout")) as Box<dyn Read + Send>,
            MAX_STDOUT,
        ),
        (
            1,
            Box::new(child.stderr.take().expect("piped stderr")) as Box<dyn Read + Send>,
            MAX_STDERR,
        ),
    ] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe
                .take((limit + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = sender.send((index, result));
        });
    }
    drop(sender);
    let deadline = Instant::now() + timeout;
    let mut outputs = [None, None];
    let mut status = None;
    let result = 'wait: loop {
        while let Ok((index, result)) = receiver.try_recv() {
            match result {
                Ok(bytes) if bytes.len() <= if index == 0 { MAX_STDOUT } else { MAX_STDERR } => {
                    outputs[index] = Some(bytes)
                }
                Ok(_) => break 'wait Err("gh output exceeded the size limit".to_owned()),
                Err(error) => break 'wait Err(format!("could not read gh output: {error}")),
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(error) => break Err(format!("could not wait for gh: {error}")),
            }
        }
        if let (Some(status), Some(stdout), Some(stderr)) =
            (status, outputs[0].as_ref(), outputs[1].as_ref())
        {
            if status.success() {
                break Ok(stdout.clone());
            }
            let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
            let message = if stderr.contains("auth")
                || stderr.contains("login")
                || stderr.contains("token")
            {
                "gh authentication failed; check GitHub CLI authentication"
            } else {
                "gh request failed; check GitHub CLI access and network connection"
            };
            break Err(message.to_owned());
        }
        if Instant::now() >= deadline {
            break Err("gh request timed out".to_owned());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if result.is_err() {
        #[cfg(unix)]
        unsafe {
            libc::killpg(child.id() as i32, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn bounds_output_and_stops_timed_out_processes() {
        let mut noisy = Command::new("sh");
        noisy.args(["-c", "while :; do printf '%01024d' 0; done"]);
        assert_eq!(
            capture(&mut noisy, Duration::from_secs(3)).unwrap_err(),
            "gh output exceeded the size limit"
        );
        let start = Instant::now();
        let mut slow = Command::new("sh");
        slow.args(["-c", "sleep 5"]);
        assert_eq!(
            capture(&mut slow, Duration::from_millis(50)).unwrap_err(),
            "gh request timed out"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

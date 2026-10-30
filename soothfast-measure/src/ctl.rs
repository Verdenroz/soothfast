//! Control channel between the CLI side of a callgrind window measurement and
//! the bench process under valgrind: two pipes, named to the bench process in
//! an environment variable, so its own stdin and stdout stay its own.
//!
//! The bench process sends one byte when the warm-up is dumped, and the CLI
//! side answers with the window size as eight little-endian bytes. A side that
//! dies closes its ends, so the other sees end of file instead of hanging.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

const ENV: &str = "SOOTHFAST_CALLGRIND_CTL";

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 returned these as new descriptors only this call owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn keep_across_exec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor this process holds.
    match unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

/// Both pipes, before the bench process is spawned.
pub struct Control {
    child_read: OwnedFd,
    child_write: OwnedFd,
    parent: Parent,
}

/// The CLI side of the channel.
pub struct Parent {
    to_child: File,
    from_child: File,
}

impl Control {
    pub fn new() -> io::Result<Control> {
        let (child_read, to_child) = pipe()?;
        let (from_child, child_write) = pipe()?;
        Ok(Control {
            child_read,
            child_write,
            parent: Parent {
                to_child: File::from(to_child),
                from_child: File::from(from_child),
            },
        })
    }

    /// Start `cmd` with the bench side's ends open and named in its
    /// environment. Every other descriptor of this process stays close-on-exec.
    pub fn attach(&self, cmd: &mut Command) {
        let (read, write) = (self.child_read.as_raw_fd(), self.child_write.as_raw_fd());
        cmd.env(ENV, format!("{read},{write}"));
        // SAFETY: runs between fork and exec and only calls fcntl, which is
        // async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                keep_across_exec(read)?;
                keep_across_exec(write)
            });
        }
    }

    /// The CLI side, once `cmd` is spawned. The bench side's ends close here,
    /// so a dead bench process reads as end of file.
    pub fn into_parent(self) -> Parent {
        self.parent
    }
}

impl Parent {
    /// Block until the bench process reports its warm-up dumped.
    pub fn warmed(&mut self) -> io::Result<()> {
        self.from_child.read_exact(&mut [0])
    }

    /// Tell the bench process how many iterations to measure.
    pub fn send_window(&mut self, window: u64) -> io::Result<()> {
        self.to_child.write_all(&window.to_le_bytes())
    }
}

/// The bench side of the channel the CLI side named in the environment.
pub struct Child {
    from_parent: File,
    to_parent: File,
}

impl Child {
    pub fn from_env() -> Result<Child, String> {
        let named = std::env::var(ENV).map_err(|_| format!("{ENV} is not set"))?;
        let fds = named
            .split_once(',')
            .and_then(|(r, w)| Some((r.parse::<RawFd>().ok()?, w.parse::<RawFd>().ok()?)))
            .ok_or_else(|| format!("{ENV} is not two descriptors: {named:?}"))?;
        // SAFETY: the parent opened these descriptors for this process and
        // named them here; nothing else in it uses them.
        let (from_parent, to_parent) =
            unsafe { (File::from_raw_fd(fds.0), File::from_raw_fd(fds.1)) };
        Ok(Child {
            from_parent,
            to_parent,
        })
    }

    /// Report the warm-up dumped and wait for the window size.
    pub fn warmed(&mut self) -> io::Result<u64> {
        self.to_parent.write_all(&[1])?;
        let mut window = [0; 8];
        self.from_parent.read_exact(&mut window)?;
        Ok(u64::from_le_bytes(window))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    use super::{Child, Control};

    const AS_BENCH: &str = "SOOTHFAST_CTL_TEST_AS_BENCH";

    // Re-run by `a_bench_that_uses_stdin_and_stdout_cannot_disturb_the_handshake`
    // as the bench process; a normal test run skips it.
    #[test]
    fn bench_process_body() {
        if std::env::var_os(AS_BENCH).is_none() {
            return;
        }
        let mut stdin = String::new();
        std::io::stdin().read_to_string(&mut stdin).unwrap();
        println!("soothfast-warm");
        println!("{} bytes of stdin", stdin.len());
        let window = Child::from_env().unwrap().warmed().unwrap();
        println!("window {window}");
    }

    #[test]
    fn a_bench_that_uses_stdin_and_stdout_cannot_disturb_the_handshake() {
        let control = Control::new().unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "ctl::tests::bench_process_body", "--nocapture"])
            .env(AS_BENCH, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        control.attach(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let mut parent = control.into_parent();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"not a window")
            .unwrap();

        parent.warmed().unwrap();
        parent.send_window(4).unwrap();

        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert!(stdout.contains("soothfast-warm\n"), "{stdout}");
        assert!(stdout.contains("12 bytes of stdin\n"), "{stdout}");
        assert!(stdout.contains("window 4\n"), "{stdout}");
    }

    #[test]
    fn a_bench_that_exits_before_the_warm_up_reads_as_end_of_file() {
        let control = Control::new().unwrap();
        let mut cmd = Command::new("true");
        control.attach(&mut cmd);
        cmd.spawn().unwrap().wait().unwrap();
        assert!(control.into_parent().warmed().is_err());
    }
}

#[cfg(windows)]
use std::env;
use std::io;
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use std::process::{Command, Stdio};

#[cfg(windows)]
mod lifetime {
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct OwnedJob(HANDLE);

    impl OwnedJob {
        pub fn create() -> io::Result<Self> {
            // Unnamed, non-inherited handle is owned by this Rust process only.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        pub fn attach(&self, child: &Child) -> io::Result<()> {
            let assigned =
                unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle().cast()) };
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for OwnedJob {
        fn drop(&mut self) {
            // Closing our one job handle also terminates remaining owned descendants.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(not(windows))]
fn run() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows lifetime PoC only",
    ))
}

#[cfg(windows)]
fn run() -> io::Result<()> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() < 3 || !Path::new(&args[0]).is_absolute() || !Path::new(&args[1]).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "absolute runtime/script and cwd required",
        ));
    }
    {
        use std::os::windows::process::CommandExt;
        let job = lifetime::OwnedJob::create()?;
        let mut command = Command::new(&args[0]);
        command.arg(&args[1]).args(&args[3..]).current_dir(&args[2]);
        command
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        for (name, value) in env::vars_os() {
            if matches!(
                name.to_string_lossy().to_ascii_lowercase().as_str(),
                "systemroot" | "windir" | "temp" | "tmp"
            ) {
                command.env(name, value);
            }
        }
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command.spawn()?;
        if let Err(error) = job.attach(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        // The minimal bootstrap imports the SDK only after this ownership barrier.
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(b"start\n")?;
        let mut control = child.stdin.take().expect("piped stdin");
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut control);
        });
        let status = child.wait()?;
        if !status.success() {
            return Err(io::Error::other("owned child failed"));
        }
        Ok(())
    }
}

fn main() {
    if run().is_err() {
        // Keep loader errors and environment values out of logs.
        std::process::exit(2);
    }
}

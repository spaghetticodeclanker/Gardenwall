use std::env;
use std::fs::OpenOptions;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(lock_path) = env::args_os().nth(1) else {
        eprintln!("Expected a lock-file path");
        return ExitCode::FAILURE;
    };

    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!("Unable to open lock file: {error}");
            return ExitCode::FAILURE;
        }
    };

    match file.try_lock().map_err(std::io::Error::from) {
        Ok(()) => {
            println!("LOCK_ACQUIRED");
            ExitCode::SUCCESS
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || (cfg!(windows) && error.raw_os_error() == Some(33)) =>
        {
            println!("LOCK_BUSY");
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("Lock acquisition failed: {error}");
            ExitCode::FAILURE
        }
    }
}

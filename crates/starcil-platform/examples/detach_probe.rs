use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use starcil_platform::spawn_detached;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        Some(mode) if mode == "--child" => {
            let marker = PathBuf::from(args.next().ok_or("missing marker path")?);
            thread::sleep(Duration::from_millis(1_500));
            std::fs::write(marker, b"detached-child-completed")?;
        }
        Some(marker) => {
            let executable = std::env::current_exe()?;
            let mut command = Command::new(executable);
            command.arg("--child").arg(marker);
            let child = spawn_detached(&mut command)?;
            println!("detached child pid={}", child.id());
        }
        None => return Err("usage: detach_probe <marker-path>".into()),
    }
    Ok(())
}

use std::{
    env,
    fs::File,
    io::{Read, Write},
    process::Command,
};

use uuid::Uuid;

pub fn open_in_editor(contents: &str, use_editor: Option<String>) -> eyre::Result<String> {
    let editor = use_editor
        .or_else(|| env::var("EDITOR").ok())
        .unwrap_or("vi".to_string());
    let mut tmp_filepath = env::temp_dir();
    tmp_filepath.push(format!("cw_query_{}.lq", Uuid::new_v4()));

    let mut tmp_file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .open(&tmp_filepath)
        .expect("File does not exist");

    tmp_file.write_all(contents.as_bytes())?;
    tmp_file.flush()?;

    // Get handle to the TTY attached to current terminal session.
    let tty = File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .expect("Failed to open /dev/tty");

    // TODO: add check to see if status code indicates success
    let _result = Command::new(editor)
        .arg(&tmp_filepath)
        .stdin(tty.try_clone().expect("Failed to clone /dev/tty for stdin"))
        .stdout(
            tty.try_clone()
                .expect("Failed to clone /dev/tty for stdout"),
        )
        .stderr(tty)
        .status()?;

    // NOTE: Reopening the file to ensure I pick up the changes written to disk by the EDITOR
    // I could just use fsync and force the os to sync the file descriptor. But this would
    // require me to add libc and be incompatible with Windows. This problem only exists on unix
    // based systems. I think
    let mut tmp_file = File::open(&tmp_filepath).expect("File should still exist");
    let mut contents = String::new();
    tmp_file.read_to_string(&mut contents)?;
    Ok(contents)
}

use std::fs;

const SOCKET_DIR: &str = "/tmp";
const PREFIX: &str = "murmur-";
const SUFFIX: &str = ".sock";

pub fn ls() -> anyhow::Result<()> {
    for entry in fs::read_dir(SOCKET_DIR)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(PREFIX) && name.ends_with(SUFFIX) {
            let channel = &name[PREFIX.len()..name.len() - SUFFIX.len()];
            println!("{}", channel);
        }
    }
    Ok(())
}

pub fn rm(channel: &str) -> anyhow::Result<()> {
    let path = crate::socket::socket_path(channel)?;
    if path.exists() {
        fs::remove_file(&path)?;
        eprintln!("removed {}", path.display());
    } else {
        eprintln!("channel '{}' not found", channel);
    }
    Ok(())
}

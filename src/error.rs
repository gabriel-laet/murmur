use std::fmt;

#[derive(Debug)]
pub enum MurmurError {
    MessageTooLarge(usize),
    InvalidChannel(String),
}

impl fmt::Display for MurmurError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageTooLarge(size) => {
                write!(f, "message too large: {} bytes (max 1MB)", size)
            }
            Self::InvalidChannel(name) => {
                write!(f, "invalid channel name: {}", name)
            }
        }
    }
}

impl std::error::Error for MurmurError {}

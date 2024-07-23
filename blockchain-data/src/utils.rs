use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn get_timestamp() -> u64 {
    let start = SystemTime::now();

    match start.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => 0,
    }
}

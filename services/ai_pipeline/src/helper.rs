use std::fs;
use std::path::{Path, PathBuf};

pub fn next_experiment_dir(base: &Path) -> PathBuf {
    fs::create_dir_all(base).ok();

    let mut max_id = 0usize;

    if let Ok(entries) = fs::read_dir(base) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                // Expect names like "experiment_1", "experiment_02", etc.
                if let Some(rest) = name.strip_prefix("experiment_")
                    && let Ok(id) = rest.parse::<usize>()
                    && id > max_id
                {
                    max_id = id;
                }
            }
        }
    }

    let next_id = max_id + 1;
    base.join(format!("experiment_{}", next_id))
}

pub fn latest_experiment_dir(base: &Path) -> Option<PathBuf> {
    let mut max_id = 0usize;
    let mut best: Option<PathBuf> = None;

    if let Ok(entries) = fs::read_dir(base) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && let Some(rest) = name.strip_prefix("experiment_")
                && let Ok(id) = rest.parse::<usize>()
                && id > max_id
            {
                max_id = id;
                best = Some(entry.path());
            }
        }
    }

    best
}

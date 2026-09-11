use std::fs;

pub fn scan() {
    let _ = fs::read_dir(".");
}

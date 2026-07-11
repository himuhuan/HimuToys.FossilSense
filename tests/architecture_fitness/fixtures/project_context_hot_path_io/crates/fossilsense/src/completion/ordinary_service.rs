use std::fs;

pub fn illegal_hot_path_marker_read() {
    let _ = fs::read_dir(".");
}

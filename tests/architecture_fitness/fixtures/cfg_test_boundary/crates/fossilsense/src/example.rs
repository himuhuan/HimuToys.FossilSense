#[cfg(test)]
fn standalone_test_helper() {}

pub fn production_boundary() {
    let one = 1;
    let two = 2;
    let three = 3;
    let four = 4;
}

#[cfg(test)]
mod tests {
    #[test]
    fn braces_in_literals_do_not_end_module() {
        let closing = "}";
        let raw = r#"}"#;
        assert_eq!(closing, raw);
    }
}

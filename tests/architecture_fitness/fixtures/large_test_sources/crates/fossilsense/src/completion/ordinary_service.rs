pub fn production_boundary() {}

#[cfg(test)]
mod tests {
    #[test]
    fn inline_test_lines_do_not_count_as_production_size() {
        let values = [
            1,
            2,
            3,
            4,
            5,
        ];
        assert_eq!(values.len(), 5);
    }
}

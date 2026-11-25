mod subject;

#[test]
fn transform() {
    let cases = [
            (-2, 2),
            (-1, 1),
            (0, 0),
            (1, 2),
            (2, 4),
            (3, 6)
        ];
    for (value, expected) in cases {
        assert_eq!(subject::transform(value), expected);
    }
}

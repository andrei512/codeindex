mod subject;

#[test]
fn transform() {
    for value in [-2, -1, 0, 1, 2, 3] {
        if value < 0 {
            assert_eq!(subject::transform(value), -value);
        } else {
            assert_eq!(subject::transform(value), value * 2);
        }
    }
}

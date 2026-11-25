mod subject;

#[test]
fn negative() {
    assert_eq!(subject::transform(-2), 2);
    assert_eq!(subject::transform(-1), 1);
}

#[test]
fn nonnegative() {
    assert_eq!(subject::transform(0), 0);
    assert_eq!(subject::transform(1), 2);
    assert_eq!(subject::transform(2), 4);
    assert_eq!(subject::transform(3), 6);
}

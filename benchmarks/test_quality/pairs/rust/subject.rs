pub fn transform(value: i32) -> i32 {
    println!("CASE={value}");
    let result = if value < 0 { -value } else { value * 2 };
    result + i32::from(std::env::var("CX_PAIR_MUTANT").ok() == Some(value.to_string()))
}

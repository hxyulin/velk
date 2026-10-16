pub fn wrong_return_type() -> usize {
    "this should be a number"
}

pub fn wrong_argument_type() {
    let words = ["one", "two", "three"];
    let _word = words.get("second");
}

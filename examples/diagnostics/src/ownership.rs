pub fn use_after_move() {
    let text = String::from("diagnostics demo");
    let moved = text;
    println!("{text}");
    drop(moved);
}

pub fn conflicting_borrows() {
    let mut text = String::from("borrowed");
    let original = &text;
    text.push_str(" again");
    println!("{original}");
}

pub trait Display {}

pub trait Error: Display {
    fn description(&self) -> &str;
}

pub struct MyError {
    code: u32,
}

impl Error for MyError {
    fn description(&self) -> &str {
        "e"
    }
}

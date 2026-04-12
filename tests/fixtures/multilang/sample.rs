pub fn greet(name: &str) -> String {
    format!("Hello, {}", name)
}

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub async fn fetch_data() -> String {
    String::new()
}

pub struct Config {
    pub host: String,
    pub port: u16,
}

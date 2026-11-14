//! Reads a command script on stdin and prints the engine's answers. Used to
//! compare this engine against the original C++ one key for key.
#[path = "../tests/common/driver.rs"]
mod driver;

use std::io::Read;

fn main() {
    let mut script = String::new();
    std::io::stdin().read_to_string(&mut script).unwrap();
    print!("{}", driver::run(&script, std::path::Path::new(".")));
}

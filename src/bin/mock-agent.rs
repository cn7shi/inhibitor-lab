use std::io::{self, Write};

fn main() {
    match std::env::args().nth(1).as_deref().unwrap_or("repeat") {
        "normal" => {
            println!("INFO starting task");
            eprintln!("INFO diagnostic stream is not automatically an error");
            println!("INFO task completed");
        }
        "repeat" => {
            println!("INFO starting task");
            for _ in 0..3 {
                eprintln!("ERROR simulated upstream unavailable");
            }
            println!("INFO finished; the observer did not stop this process");
        }
        "fail" => {
            eprintln!("ERROR simulated child failure");
            std::process::exit(7);
        }
        "long-line" => {
            println!("{}ERROR", "x".repeat(70 * 1024));
            print!("final line without newline");
            io::stdout().flush().unwrap();
        }
        other => {
            eprintln!("unknown scenario: {other}; use normal, repeat, fail, or long-line");
            std::process::exit(2);
        }
    }
}

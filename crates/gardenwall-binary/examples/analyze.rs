//! `cargo run -p gardenwall-binary --example analyze -- <file> <op> [key=value...]`
//! runs the analyzer directly (no sandbox), for development only.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).expect("read");
    let op = match args.get(1).map(String::as_str).unwrap_or("info") {
        "functions" => gardenwall_binary::Op::Functions {
            offset: 0,
            limit: 30,
            filter: args.get(2).cloned(),
        },
        "disasm" => gardenwall_binary::Op::Disasm {
            target: args.get(2).cloned().unwrap_or("entry".into()),
            count: 60,
        },
        "strings" => gardenwall_binary::Op::Strings {
            min_len: 6,
            offset: 0,
            limit: 40,
            filter: args.get(2).cloned(),
        },
        _ => gardenwall_binary::Op::Info,
    };
    match gardenwall_binary::run(&bytes, &op) {
        Ok(t) => print!("{t}"),
        Err(e) => eprintln!("error: {e}"),
    }
}

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "dumping is the example purpose"
)]

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let source = std::fs::read_to_string(&path).expect("read");
    let out =
        rustel_transpiler::transpile(&source, &rustel_transpiler::TranspileOptions::default());
    for d in &out.diagnostics {
        eprintln!("DIAG {:?} {}", d.offset, d.message);
    }
    println!("{}", out.output);
}

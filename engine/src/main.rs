mod bench;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("bench") => bench::run(&args[1..]),
        _ => {
            eprintln!("usage: powerdb bench [--records N] [--big-mib N] [--sync]");
            2
        }
    };
    std::process::exit(code);
}

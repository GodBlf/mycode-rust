const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let Some(argument) = std::env::args().nth(1) else {
        print_usage();
        return;
    };

    match argument.as_str() {
        "--version" | "-V" => println!("mycode {VERSION}"),
        "--help" | "-h" => print_usage(),
        _ => print_usage(),
    }
}

fn print_usage() {
    println!("MyCode terminal coding agent");
    println!();
    println!("Usage: mycode [OPTIONS]");
    println!();
    println!("Options:");
    println!("  -h, --help     Print help");
    println!("  -V, --version  Print version");
}

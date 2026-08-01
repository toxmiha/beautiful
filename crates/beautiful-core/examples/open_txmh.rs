fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("path");
    match beautiful_core::load_txmh(std::path::Path::new(path)) {
        Ok(d) => println!("OK {}x{} layers={}", d.width, d.height, d.layers.len()),
        Err(e) => {
            eprintln!("FAIL: {e}");
            std::process::exit(1);
        }
    }
}

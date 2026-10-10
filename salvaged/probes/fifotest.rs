fn main() {
    let p = std::env::args().nth(1).unwrap();
    let meta = std::fs::metadata(&p).unwrap();
    println!("metadata().len() = {}  (is_fifo-ish: {})", meta.len(), !meta.is_file());
    match rustyn64_frontend::romfile::read_rom(std::path::Path::new(&p)) {
        Ok(v) => println!("READ {} bytes -- UNBOUNDED", v.len()),
        Err(e) => println!("REFUSED: {e}"),
    }
}

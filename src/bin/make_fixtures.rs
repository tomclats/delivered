use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    if let Err(err) = delivered::write_fixtures(&root) {
        eprintln!("make-fixtures: {err}");
        std::process::exit(1);
    }
    println!("wrote {}", root.display());
}

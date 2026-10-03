//! Stamps the commit this binary was built from, and lists the bundled `!!!ClassicAPI` addon's
//! files for `include_bytes!`.

use std::fmt::Write as _;
use std::path::Path;

/// Every file under `dir`, as `/`-separated paths relative to `root`, sorted.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(
                rel.to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}

fn main() {
    benilla_buildstamp::emit();
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let root = Path::new(&manifest).join("addon").join("!!!ClassicAPI");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    let mut src = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for f in &files {
        let _ = writeln!(
            src,
            "    ({f:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/addon/!!!ClassicAPI/{f}\"))),"
        );
    }
    src.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("embedded_addon.rs");
    std::fs::write(out, src).expect("write embedded_addon.rs");
}

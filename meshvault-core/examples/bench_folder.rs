use meshvault_core::chunk::chunk_stream;
use std::fs::File;
use std::path::Path;
use std::time::Instant;

fn visit(dir: &Path, bytes: &mut u64, chunks: &mut u64) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            visit(&entry.path(), bytes, chunks)?;
        } else if kind.is_file() {
            for chunk in chunk_stream(File::open(entry.path())?) {
                let (c, _) = chunk?;
                *bytes += u64::from(c.len);
                *chunks += 1;
            }
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: bench_folder <dir>");
        std::process::exit(2);
    };
    let (mut bytes, mut chunks) = (0u64, 0u64);
    let start = Instant::now();
    visit(Path::new(&dir), &mut bytes, &mut chunks)?;
    let secs = start.elapsed().as_secs_f64();
    let mb = bytes as f64 / 1_000_000.0;
    println!("{mb:.1} MB in {secs:.2} s = {:.0} MB/s", mb / secs);
    if chunks > 0 {
        println!(
            "{chunks} chunks, average {:.0} KiB",
            bytes as f64 / chunks as f64 / 1024.0
        );
    }
    Ok(())
}

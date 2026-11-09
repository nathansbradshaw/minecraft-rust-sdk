//! Reproducible std-only compile load, separate from gameplay and model access.
use std::{fs, io::Write, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("fixture-workload NEW_DIRECTORY".into());
    }
    let root = Path::new(&args[0]);
    fs::create_dir(root)?;
    fs::create_dir(root.join("src"))?;
    fs::create_dir(root.join("src/bin"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='body-compile-fixture'\nversion='0.0.0'\nedition='2024'\npublish=false\n[workspace]\n[profile.dev]\ndebug=0\nstrip='debuginfo'\nincremental=false\n",
    )?;
    for bin in 0..10 {
        let mut source = fs::File::create(root.join(format!("src/bin/load_{bin}.rs")))?;
        writeln!(
            source,
            "#[inline(never)] fn work<const N: usize>(mut x:u64)->u64 {{ x=x.wrapping_add(N as u64); for _ in 0..32 {{ x=std::hint::black_box(x.rotate_left(7).wrapping_mul(6364136223846793005)); }} x }}"
        )?;
        writeln!(
            source,
            "fn main() {{ let mut x=std::hint::black_box({bin}u64);"
        )?;
        for function in 0..1024 {
            writeln!(source, "x=work::<{function}>(x);")?;
        }
        writeln!(source, "println!(\"{{x}}\"); }}")?;
    }
    println!(
        "Created ten std-only binaries, each with1024 non-inlined generic instantiations. Build with cargo build --offline; no SDK/model/gameplay calls. Fresh target required for a cold-workload observation; retained targets are warm evidence."
    );
    Ok(())
}

//! Bounded Rust gzip/ustar import, deliberately refusing links and extensions.
use super::*;
use flate2::read::MultiGzDecoder;
fn text(bytes: &[u8]) -> Result<&str> {
    Ok(std::str::from_utf8(
        bytes.split(|b| *b == 0).next().unwrap_or_default(),
    )?)
}
fn octal(bytes: &[u8]) -> Result<u64> {
    let s = text(bytes)?.trim();
    ensure(
        !s.is_empty() && s.bytes().all(|b| matches!(b, b'0'..=b'7')),
        "ustar numeric field required",
    )?;
    Ok(u64::from_str_radix(s, 8)?)
}
fn header(block: &[u8; 512]) -> Result<(PathBuf, u64, bool)> {
    ensure(
        &block[257..263] == b"ustar\0" || &block[257..263] == b"ustar ",
        "normalized ustar archive required",
    )?;
    let sum: u64 = block
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(*b)
            }
        })
        .sum();
    ensure(sum == octal(&block[148..156])?, "ustar checksum differs")?;
    let name = text(&block[..100])?;
    let prefix = text(&block[345..500])?;
    let path = if prefix.is_empty() {
        PathBuf::from(name)
    } else {
        Path::new(prefix).join(name)
    };
    ensure(
        relative(&path) && path.components().count() <= 32 && path.as_os_str().len() <= 512,
        "unsafe archive path",
    )?;
    ensure(
        matches!(block[156], 0 | b'0' | b'5') && text(&block[157..257])?.is_empty(),
        "archive links, devices and extensions are forbidden",
    )?;
    let size = octal(&block[124..136])?;
    let dir = block[156] == b'5';
    ensure(
        size <= 64 * 1024 * 1024 && (!dir || size == 0),
        "archive entry exceeds bound",
    )?;
    Ok((path, size, dir))
}
pub(super) fn extract(
    source: &Path,
    destination: &Path,
    roots: &[String],
    remaining: u64,
) -> Result<u64> {
    extract_bounded(source, destination, roots, remaining, || {
        free_bytes(destination)
    })
}
#[cfg(test)]
pub(super) fn extract_with_space(
    source: &Path,
    destination: &Path,
    roots: &[String],
    available: impl FnMut() -> Result<u64>,
) -> Result<u64> {
    extract_bounded(
        source,
        destination,
        roots,
        4 * 1024 * 1024 * 1024,
        available,
    )
}
pub(super) fn extract_bounded(
    source: &Path,
    destination: &Path,
    roots: &[String],
    ceiling: u64,
    mut available: impl FnMut() -> Result<u64>,
) -> Result<u64> {
    let mut input = MultiGzDecoder::new(fs::File::open(source)?);
    let mut seen = BTreeSet::new();
    let mut found = BTreeSet::new();
    let mut total = 0u64;
    let mut zero = 0;
    for _ in 0..32768 {
        ensure(!take_interrupt(), "preparation archive import interrupted")?;
        let mut block = [0; 512];
        input.read_exact(&mut block)?;
        if block.iter().all(|b| *b == 0) {
            zero += 1;
            if zero == 2 {
                let mut tail = [0; 8192];
                loop {
                    let n = input.read(&mut tail)?;
                    if n == 0 {
                        break;
                    }
                    ensure(tail[..n].iter().all(|b| *b == 0), "trailing archive data")?;
                    total = total.saturating_add(n as u64);
                    ensure(total <= ceiling, "archive expands beyond bound")?;
                }
                ensure(
                    roots.iter().all(|r| found.contains(r)),
                    "declared archive root absent",
                )?;
                return Ok(total);
            }
            continue;
        }
        ensure(zero == 0, "unexpected archive data after end marker")?;
        let (path, size, dir) = header(&block)?;
        let first = path
            .components()
            .next()
            .unwrap()
            .as_os_str()
            .to_str()
            .ok_or("non-UTF8 archive root")?
            .to_owned();
        ensure(
            roots.contains(&first) && seen.insert(path.clone()),
            "undeclared or duplicate archive entry",
        )?;
        found.insert(first);
        total = total.saturating_add(512 + size.div_ceil(512) * 512);
        ensure(
            total <= ceiling,
            "archive expands beyond remaining byte budget",
        )?;
        let target = destination.join(path);
        parents(destination, target.parent().unwrap())?;
        if dir {
            if !target.exists() {
                mkdir(&target)?;
            }
            directory(&target)?;
        } else {
            ensure(
                available()? >= 500_000_000_000 + size,
                "archive write would cross disk reserve",
            )?;
            let mut file = create(&target)?;
            let mut limited = (&mut input).take(size);
            ensure(
                std::io::copy(&mut limited, &mut file)? == size,
                "short archive entry",
            )?;
            file.sync_all()?;
        }
        let padding = (512 - size % 512) % 512;
        let mut pad = [0; 512];
        input.read_exact(&mut pad[..padding as usize])?;
        ensure(
            pad[..padding as usize].iter().all(|b| *b == 0),
            "nonzero archive padding",
        )?;
    }
    Err("archive entry limit exceeded".into())
}

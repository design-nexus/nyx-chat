//! How much memory there is for models, and whether a download will fit.
//!
//! A sleeping discrete GPU is never woken to ask: NVIDIA memory is read with
//! `nvidia-smi` only while the card is awake, and remembered for next time.

use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Memory {
    /// Dedicated GPU memory (0 when there's no discrete GPU we can read).
    pub vram: u64,
    pub ram: u64,
}

/// `cached_vram` is what Nyx AI Core last reported; with local models in AI Core
/// that's the figure to fit against, so the card isn't probed here at all.
pub fn detect(cached_vram: u64) -> Memory {
    if crate::llama::api::local_service() && cached_vram > 0 {
        return Memory { vram: cached_vram, ram: ram() };
    }
    let vram = amd_vram().or_else(nvidia_vram).or((cached_vram > 0).then_some(cached_vram)).or_else(nvidia_bar).unwrap_or(0);
    Memory { vram, ram: ram() }
}

/// An NVIDIA card's memory window (BAR 1) from sysfs, which doesn't wake it.
/// With resizable BAR, as on current cards, it's the size of the memory.
fn nvidia_bar() -> Option<u64> {
    let entries = std::fs::read_dir("/sys/bus/pci/devices").ok()?;
    entries
        .flatten()
        .filter(|e| {
            let vendor = std::fs::read_to_string(e.path().join("vendor")).unwrap_or_default();
            let class = std::fs::read_to_string(e.path().join("class")).unwrap_or_default();
            vendor.trim() == "0x10de" && class.starts_with("0x03")
        })
        .filter_map(|e| bar1_size(&std::fs::read_to_string(e.path().join("resource")).ok()?))
        .max()
}

fn bar1_size(resource: &str) -> Option<u64> {
    let line = resource.lines().nth(1)?;
    let mut parts = line.split_whitespace().map(|x| u64::from_str_radix(x.trim_start_matches("0x"), 16).ok());
    let (start, end) = (parts.next()??, parts.next()??);
    let size = end.checked_sub(start)? + 1;
    // Small windows (256 MB without resizable BAR) say nothing about the memory.
    (2 << 30..=512 << 30).contains(&size).then_some(size)
}

fn ram() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| {
            t.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

fn amd_vram() -> Option<u64> {
    let entries = std::fs::read_dir("/sys/class/drm").ok()?;
    entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("device/mem_info_vram_total")).ok()?.trim().parse::<u64>().ok())
        // An APU reports a small carve-out; only count real cards.
        .filter(|v| *v >= 2 << 30)
        .max()
}

fn nvidia_vram() -> Option<u64> {
    let entries = std::fs::read_dir("/sys/bus/pci/devices").ok()?;
    let awake = entries.flatten().any(|e| {
        let vendor = std::fs::read_to_string(e.path().join("vendor")).unwrap_or_default();
        let class = std::fs::read_to_string(e.path().join("class")).unwrap_or_default();
        vendor.trim() == "0x10de" && class.starts_with("0x03") && runtime_active(&e.path())
    });
    if !awake || !crate::cmd::present("nvidia-smi") {
        return None;
    }
    let out = crate::cmd::output(&["nvidia-smi", "--query-gpu=memory.total", "--format=csv,noheader,nounits"])?;
    out.lines().filter_map(|l| l.trim().parse::<u64>().ok()).max().map(|mib| mib * 1024 * 1024)
}

fn runtime_active(dev: &Path) -> bool {
    std::fs::read_to_string(dev.join("power/runtime_status")).map(|s| s.trim() != "suspended").unwrap_or(true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Gpu,
    Partial,
    TooBig,
}

impl Fit {
    pub fn label(self) -> &'static str {
        match self {
            Fit::Gpu => "Fits GPU",
            Fit::Partial => "Partial offload",
            Fit::TooBig => "Too big",
        }
    }
}

/// Memory a model needs: weights, plus the KV cache for `ctx` tokens, plus
/// runtime overhead. KV per token grows more slowly than the weights (bigger
/// models add layers, not KV heads): about 130 KB per token for both an 8B and
/// a 27B grouped-query model at fp16, which the square root follows.
pub fn needed(file_size: u64, ctx: u64) -> u64 {
    needed_with_kv(file_size, ctx, "f16")
}

pub fn needed_with_kv(file_size: u64, ctx: u64, kv_type: &str) -> u64 {
    let gb = file_size as f64 / 1e9;
    let ratio = match kv_type { "q8_0" => 0.53125, "q4_0" => 0.28125, _ => 1.0 };
    let kv = ctx as f64 * 45_000.0 * gb.max(0.5).sqrt() * ratio;
    file_size + kv as u64 + 500_000_000
}

pub fn fit(file_size: u64, ctx: u64, mem: Memory) -> Fit {
    fit_with_kv(file_size, ctx, "f16", mem)
}

pub fn fit_with_kv(file_size: u64, ctx: u64, kv_type: &str, mem: Memory) -> Fit {
    let need = if kv_type == "f16" { needed(file_size, ctx) } else { needed_with_kv(file_size, ctx, kv_type) };
    if mem.vram > 0 && need as f64 <= mem.vram as f64 * 0.95 {
        Fit::Gpu
    } else if need as f64 <= mem.vram as f64 + mem.ram as f64 * 0.6 {
        Fit::Partial
    } else {
        Fit::TooBig
    }
}

/// The quant to suggest: the largest common one that fits on the GPU, else
/// the best that fits at all. Returns an index into `files`.
pub fn recommend(files: &[(String, u64)], ctx: u64, mem: Memory) -> Option<usize> {
    recommend_with(files, |_, size| fit(size, ctx, mem))
}

pub fn recommend_with(files: &[(String, u64)], classify: impl Fn(usize, u64) -> Fit) -> Option<usize> {
    const GOOD: [&str; 6] = ["Q8_0", "Q6_K", "Q5_K_M", "Q4_K_M", "IQ4_XS", "Q3_K_M"];
    let pick =
        |want: Fit| GOOD.iter().find_map(|q| files.iter().enumerate().find(|(i, (quant, size))| quant == q && *size > 0 && classify(*i, *size) == want).map(|(i, _)| i));
    pick(Fit::Gpu).or_else(|| pick(Fit::Partial)).or_else(|| {
        [Fit::Gpu, Fit::Partial].into_iter().find_map(|want| {
            files.iter().enumerate().filter(|(i, (_, size))| *size > 0 && classify(*i, *size) == want)
                .max_by_key(|(_, (_, size))| *size).map(|(i, _)| i)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    #[test]
    fn fits() {
        let mem = Memory { vram: 8 * GB, ram: 32 * GB };
        assert_eq!(fit(4_900_000_000, 8192, mem), Fit::Gpu);
        assert_eq!(fit(16 * GB, 8192, mem), Fit::Partial);
        assert_eq!(fit(40 * GB, 8192, mem), Fit::TooBig);
        // A 16 GB card: a 12 GB file fits, a 16.5 GB 27B Q4 doesn't quite.
        let gpu16 = Memory { vram: 17_094_934_528, ram: 33 * GB };
        assert_eq!(fit(12 * GB, 8192, gpu16), Fit::Gpu);
        assert_eq!(fit(16_500_000_000, 8192, gpu16), Fit::Partial);
        let cpu_only = Memory { vram: 0, ram: 16 * GB };
        assert_eq!(fit(4 * GB, 4096, cpu_only), Fit::Partial);
        assert!(needed_with_kv(4 * GB, 65536, "q4_0") < needed_with_kv(4 * GB, 65536, "q8_0"));
        assert!(needed_with_kv(4 * GB, 65536, "q8_0") < needed(4 * GB, 65536));
    }

    #[test]
    fn reads_bar1() {
        let res = "0x00000000a3000000 0x00000000a3ffffff 0x0000000000040200\n0x0000006000000000 0x00000063ffffffff 0x000000000014220c\n";
        assert_eq!(bar1_size(res), Some(16 << 30));
        let small = "0x0 0x0 0x0\n0x00000000e0000000 0x00000000efffffff 0x0\n";
        assert_eq!(bar1_size(small), None);
    }

    #[test]
    fn recommends_biggest_that_fits() {
        let mem = Memory { vram: 8 * GB, ram: 32 * GB };
        let files = vec![
            ("Q4_K_M".to_string(), 4_900_000_000),
            ("Q5_K_M".to_string(), 5_800_000_000),
            ("Q8_0".to_string(), 8_700_000_000),
        ];
        assert_eq!(recommend(&files, 4096, mem), Some(1));
        assert_eq!(recommend(&[("Q4_0".into(), 4 * GB)], 4096, mem), Some(0));
    }
}

#[cfg(test)]
mod network_tests {
    #[test]
    #[ignore]
    fn fit_report() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mem = super::detect(0);
        println!("mem {mem:?}");
        rt.block_on(async {
            let list = crate::hf::search("", crate::hf::Sort::Trending, crate::hf::Kind::Text).await.unwrap();
            for m in list.iter().take(12) {
                let Ok(d) = crate::hf::detail(&m.id).await else { continue };
                for f in &d.files {
                    let fit = super::fit(f.size, 8192, mem);
                    if fit != super::Fit::Gpu {
                        println!(
                            "{} {} {} {:?} need {}",
                            m.id,
                            f.quant,
                            crate::util::size(f.size),
                            fit,
                            crate::util::size(super::needed(f.size, 8192))
                        );
                    }
                }
            }
        });
    }
}

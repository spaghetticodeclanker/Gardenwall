//! Print the GPU memory status: `cargo run -p gardenwall-gpu --example gpu_status`.

fn main() {
    if let Ok((total, avail)) = gardenwall_gpu::system_memory() {
        let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
        println!(
            "system RAM: {:.1} GiB, {:.1} GiB available",
            gib(total),
            gib(avail)
        );
    }
    match gardenwall_gpu::report() {
        Err(e) => eprintln!("{e}"),
        Ok(r) => {
            let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
            println!("{}: {:.1} GiB dedicated", r.adapter, gib(r.dedicated_total));
            match r.dedicated_used {
                Some(u) => println!(
                    "in use: {:.1} GiB, free {:.1} GiB",
                    gib(u),
                    gib(r.free().unwrap_or(0))
                ),
                None => println!("in use: unknown (counters unavailable)"),
            }
            println!("budget for one process: {:.1} GiB", gib(r.budget));
            println!("under pressure: {}", r.under_pressure());
            for p in r.processes.iter().take(10) {
                println!(
                    "{:>7}  {:<32} {:>6.2} GiB dedicated  {:>6.2} GiB shared",
                    p.pid,
                    if p.name.is_empty() {
                        "(no access)"
                    } else {
                        &p.name
                    },
                    gib(p.dedicated),
                    gib(p.shared)
                );
            }
        }
    }
}

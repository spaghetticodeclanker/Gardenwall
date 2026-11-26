//! Prints the main GPU's vendor and driver version (read only).
fn main() {
    match gardenwall_gpu::driver() {
        Ok(d) => println!(
            "{}: vendor {:#06x}, driver {:?}, NVIDIA version {:?}",
            d.adapter,
            d.vendor,
            d.version,
            d.nvidia_version()
        ),
        Err(e) => println!("unavailable: {e}"),
    }
}

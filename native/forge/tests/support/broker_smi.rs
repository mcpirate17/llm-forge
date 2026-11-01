//! Native slow-device fixture; never probes the host GPU.
fn main() {
    std::thread::sleep(std::time::Duration::from_millis(700));
    println!("gnome-shell, 10");
}

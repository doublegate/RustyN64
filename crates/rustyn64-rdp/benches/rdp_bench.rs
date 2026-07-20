//! Criterion bench for `rustyn64-rdp` (each chip is benchmarkable in isolation —
//! that is what the one-directional crate graph buys us).
fn main() {
    // Placeholder: link the crate so the bench harness compiles. Replace with
    // `criterion::criterion_group!/main!` once `Rdp::tick` does real work.
    println!("rustyn64-rdp {}", rustyn64_rdp::version());
}

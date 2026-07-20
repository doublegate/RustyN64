//! Criterion bench for `rustyn64-cart` (each chip is benchmarkable in isolation —
//! that is what the one-directional crate graph buys us).
fn main() {
    // Placeholder: link the crate so the bench harness compiles. Replace with
    // `criterion::criterion_group!/main!` once `Cart::tick` does real work.
    println!("rustyn64-cart {}", rustyn64_cart::version());
}

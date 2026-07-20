//! Criterion bench for `rustyn64-audio` (each chip is benchmarkable in isolation —
//! that is what the one-directional crate graph buys us).
fn main() {
    // Placeholder: link the crate so the bench harness compiles. Replace with
    // `criterion::criterion_group!/main!` once `Audio::tick` does real work.
    println!("rustyn64-audio {}", rustyn64_audio::version());
}

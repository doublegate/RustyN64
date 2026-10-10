#[test]
#[ignore]
fn cmp() {
    let b = rustyn64_test_harness::conformance::vector_bytes("tex_tri_ci4_tlut_16");
    let v = rustyn64_test_harness::conformance::parse(b);
    let got = rustyn64_test_harness::conformance::replay(&v);
    let hx = |s: &[u8]| (0..8).map(|r| (0..8).map(|c| {
        let i = (r*8+c)*2; format!("{:02x}{:02x}", s[i], s[i+1])
    }).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>();
    eprintln!("GOLDEN (Angrylion):"); for l in hx(v.golden_fb) { eprintln!("  {l}"); }
    eprintln!("OURS:");              for l in hx(&got)        { eprintln!("  {l}"); }
}

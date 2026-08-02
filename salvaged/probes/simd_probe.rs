fn main() {
    use std::simd::u16x8;
    let a = u16x8::splat(3);
    let b = u16x8::splat(4);
    println!("{:?}", a * b);
}

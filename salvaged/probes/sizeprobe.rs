#[test]
fn probe_latch_sizes() {
    use core::mem::size_of;
    println!("Latch            {}", size_of::<crate::pipeline::Latch>());
    println!("  Decoded        {}", size_of::<crate::decode::Decoded>());
    println!("  WriteBack      {}", size_of::<crate::exec::WriteBack>());
    println!("  Option<MemOp>  {}", size_of::<Option<crate::exec::MemOp>>());
    println!("  Option<Cop0>   {}", size_of::<Option<crate::cop0::Cop0Access>>());
    println!("  Option<Except> {}", size_of::<Option<crate::exception::Exception>>());
}

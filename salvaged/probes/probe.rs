    #[test]
    fn probe_partial() {
        let mut bus = Bus::new();
        let addr = 0x0010_0000u32;
        bus.rdram[addr as usize..addr as usize + 4].copy_from_slice(&0x2900_0000u32.to_be_bytes());
        bus.rdp.dpc_write(0, addr);
        bus.rdp.dpc_write(1, addr + 4);
        println!("start={:#x} current={:#x} end={:#x} status={:#x}",
            bus.rdp.dpc_read(0), bus.rdp.dpc_read(2), bus.rdp.dpc_read(1), bus.rdp.dpc_read(3));
        for _ in 0..4 { bus.rdp_tick(); }
        println!("after ticks current={:#x} processed={} tap_len={}",
            bus.rdp.dpc_read(2), bus.rdp.commands_processed, bus.rdp_tap.len());
    }

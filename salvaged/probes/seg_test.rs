    /// **R-18: a sign-extended KSEG0 address stays Direct with `KX = 1`.**
    /// Banjo-Kazooie's `SD $t0, 0x58($k0)` targets `0xFFFF_FFFF_8028_4C78` in
    /// kernel mode with 64-bit addressing enabled and takes AdES.
    #[test]
    fn r18_kseg0_is_direct_in_wide_kernel_mode() {
        let acc = Access { mode: Mode::Kernel, wide: true, erl: false };
        let v = 0xFFFF_FFFF_8028_4C78u64;
        assert!(
            matches!(segment(v, acc), Segment::Direct { .. }),
            "KSEG0 must stay direct with KX=1, got {:?}",
            segment(v, acc)
        );
        // ... and with KX clear, for contrast.
        let narrow = Access { mode: Mode::Kernel, wide: false, erl: false };
        assert!(matches!(segment(v, narrow), Segment::Direct { .. }));
    }

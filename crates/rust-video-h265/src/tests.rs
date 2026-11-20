//! Grammar tests behind synthetic bitstreams.

use crate::bits::BitReader;
use crate::error::test_support::TestError;
use crate::error::NalAction;
use crate::error::{H265Gate, ParseReason, StrictGate, UnsupportedReason};
use crate::framing::rbsp;
use crate::picture::Ingest;
use crate::pps::{parse_pps, Pps};
use crate::scaling::parse_scaling_list_data;
use crate::slice::SliceType;
use crate::sps::{parse_sps, Sps};

use rust_video_harness::Bits;

/// Test gate with ingest citation checks.
#[derive(Default)]
struct TestGate;

impl H265Gate<TestError> for TestGate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &Sps) -> Result<(), TestError> {
        let _ = sps;
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), TestError> {
        let _ = (pps, sps);
        Ok(())
    }

    /// Sequence citation of video set `vps_id`, seen when `seen`.
    fn check_sps_citation(vps_id: u8, seen: bool) -> Result<(), TestError> {
        let _ = vps_id;
        if seen {
            Ok(())
        } else {
            Err(TestError::SpsCitesUnknownVps)
        }
    }
}

/// Test gate asserting the classified NAL layer id.
#[derive(Default)]
struct LayerGate;

impl H265Gate<TestError> for LayerGate {
    /// Layer 2 expected; anything else fails the pin.
    fn classify_nal(nal_type: u8, layer_id: u16) -> Result<NalAction, TestError> {
        assert_eq!((nal_type, layer_id), (1, 2));
        Ok(NalAction::Ignore)
    }

    /// SPS check hook.
    fn check_sps(sps: &Sps) -> Result<(), TestError> {
        let _ = sps;
        Ok(())
    }

    /// PPS check hook.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), TestError> {
        let _ = (pps, sps);
        Ok(())
    }
}

/// Profile-tier-level with Main profile compatibility.
fn ptl(writer: &mut Bits, interlaced: bool) {
    writer.bits(0, 2);
    writer.bit(false);
    writer.bits(1, 5);
    for index in 0..32 {
        writer.bit(index == 1);
    }
    writer.bit(!interlaced);
    writer.bit(interlaced);
    writer.bit(true);
    writer.bit(true);
    writer.bits(0, 24);
    writer.bits(0, 19);
    writer.bit(false);
    writer.bits(120, 8);
}

/// NAL unit with two-byte header over the EBSP body.
fn nal(nal_type: u8, temporal_id: u8, body: &[u8]) -> Vec<u8> {
    let mut nal = vec![nal_type << 1, temporal_id + 1];
    nal.extend_from_slice(body);
    nal
}

/// Packet of NAL units with four-byte start codes.
fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut packet = Vec::new();
    for nal in nals {
        packet.extend_from_slice(&[0, 0, 0, 1]);
        packet.extend_from_slice(nal);
    }
    packet
}

/// 320x240 sequence set body with one explicit reference set.
fn sps_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    ptl(&mut writer, false);
    writer.ue(0);
    writer.ue(1);
    writer.ue(320);
    writer.ue(240);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.ue(4);
    writer.ue(2);
    writer.ue(5);
    writer.ue(0);
    writer.ue(3);
    writer.ue(0);
    writer.ue(3);
    writer.ue(1);
    writer.ue(1);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.ue(1);
    writer.ue(1);
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// Sequence set used across tests.
fn sps() -> Sps {
    parse_sps::<TestError, TestGate>(&rbsp(&sps_body())).expect("valid sps")
}

/// Picture set body over sequence set zero.
fn pps_body() -> Vec<u8> {
    pps_body_opts(false)
}

/// Picture set body with dependent slice selection.
fn pps_body_opts(dependent: bool) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(dependent);
    writer.bit(false);
    writer.bits(0, 3);
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.ue(1);
    writer.se(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// IDR slice header body over the test sets.
fn idr_slice_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(2);
    writer.bit(true);
    writer.bit(true);
    writer.se(0);
    writer.bit(true);
    writer.ue(0);
    writer.finish()
}

/// P slice header body with `lsb` over the test sets.
fn p_slice_body(lsb: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.ue(0);
    writer.ue(1);
    writer.bits(lsb, 4);
    writer.bit(true);
    writer.bit(true);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.se(0);
    writer.bit(true);
    writer.ue(0);
    writer.finish()
}

/// B slice header body with an explicit reference set.
fn b_slice_body(lsb: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.ue(0);
    writer.ue(0);
    writer.bits(lsb, 4);
    writer.bit(false);
    writer.bit(false);
    writer.ue(1);
    writer.ue(1);
    writer.ue(1);
    writer.bit(true);
    writer.ue(0);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.ue(0);
    writer.se(0);
    writer.bit(true);
    writer.ue(0);
    writer.finish()
}

/// CRA I slice header body with `lsb` over the test sets.
fn cra_slice_body(lsb: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(2);
    writer.bits(lsb, 4);
    writer.bit(true);
    writer.bit(true);
    writer.bit(true);
    writer.bit(true);
    writer.se(0);
    writer.bit(true);
    writer.ue(0);
    writer.finish()
}

/// Parameter set NALs: video, sequence, picture.
fn param_nals() -> Vec<Vec<u8>> {
    vec![
        nal(32, 0, &[0x00, 0x80]),
        nal(33, 0, &sps_body()),
        nal(34, 0, &pps_body()),
    ]
}

#[test]
fn sps_reports_coded_size() {
    let sps = sps();
    assert_eq!(sps.id, 0);
    assert_eq!(sps.level_idc, 120);
    assert_eq!((sps.width, sps.height), (320, 240));
    assert_eq!(sps.min_cb_size(), 8);
    assert_eq!(sps.ctb_size(), 64);
    assert_eq!(sps.width_in_ctbs(), 5);
    assert_eq!(sps.height_in_ctbs(), 4);
    assert_eq!(sps.max_poc_lsb(), 16);
    assert_eq!(sps.max_dec_pic_buffering_minus1, 4);
    assert_eq!(sps.max_num_reorder_pics, 2);
    assert!(sps.sao());
    assert!(sps.temporal_mvp());
    assert!(!sps.scaling_enabled());
    assert_eq!(sps.rps.len(), 1);
    assert_eq!(sps.rps[0].negative_count, 1);
    assert_eq!(sps.rps[0].negative[0], -1);
    assert!(sps.rps[0].negative_used[0]);
    assert_eq!(sps.visible_width(), 320);
    assert_eq!(sps.visible_height(), 240);
}

#[test]
fn sps_parses_predicted_rps() {
    let mut writer = Bits::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    ptl(&mut writer, false);
    writer.ue(0);
    writer.ue(1);
    writer.ue(320);
    writer.ue(240);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.ue(4);
    writer.ue(2);
    writer.ue(5);
    writer.ue(0);
    writer.ue(3);
    writer.ue(0);
    writer.ue(3);
    writer.ue(1);
    writer.ue(1);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(2);
    writer.ue(2);
    writer.ue(1);
    writer.ue(0);
    writer.bit(true);
    writer.ue(1);
    writer.bit(true);
    writer.ue(1);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.ue(0);
    writer.bit(true);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    let sps = parse_sps::<TestError, TestGate>(&rbsp(&writer.finish())).expect("predicted rps");
    assert_eq!(sps.rps.len(), 2);
    let predicted = &sps.rps[1];
    assert_eq!(predicted.negative_count, 3);
    assert_eq!(predicted.negative[..3], [-1, -2, -4]);
    assert_eq!(predicted.negative_used[..3], [true, true, false]);
    assert_eq!(predicted.positive_count, 0);
}

#[test]
fn pps_reports_flags_and_defaults() {
    let pps =
        parse_pps::<TestError, TestGate>(&rbsp(&pps_body()), Some(&sps())).expect("valid pps");
    assert_eq!(pps.id, 0);
    assert_eq!(pps.sps_id, 0);
    assert!(pps.sign_hiding());
    assert!(pps.weighted_pred());
    assert!(!pps.weighted_bipred());
    assert!(!pps.tiles_enabled());
    assert!(pps.entropy_sync());
    assert!(pps.loop_across_slices());
    assert!(pps.cu_qp_delta());
    assert_eq!(pps.diff_cu_qp_delta_depth, 1);
    assert_eq!(pps.log2_parallel_merge_minus2, 0);
    assert_eq!((pps.num_tile_columns, pps.num_tile_rows), (1, 1));
    assert!(
        pps.column_width.is_empty(),
        "untiled picture stores no widths"
    );
}

#[test]
fn strict_gate_holds_8bit_420() {
    let sps = sps();
    let checked: Result<(), TestError> = StrictGate::check_sps(&sps);
    assert!(checked.is_ok());
    let mut wide = sps.clone();
    wide.chroma_format = 0;
    assert_eq!(
        StrictGate::check_sps(&wide),
        Err(TestError::Parse(ParseReason::Range {
            syntax: "chroma_format_idc",
        }))
    );
    wide.chroma_format = 1;
    wide.bit_depth_luma = 2;
    assert_eq!(
        StrictGate::check_sps(&wide),
        Err(TestError::Parse(ParseReason::Range {
            syntax: "bit_depth_luma_minus8",
        }))
    );
    wide.bit_depth_luma = 0;
    wide.bit_depth_chroma = 2;
    assert_eq!(
        StrictGate::check_sps(&wide),
        Err(TestError::Parse(ParseReason::Range {
            syntax: "bit_depth_chroma_minus8",
        }))
    );
}

#[test]
fn strict_gate_declines_dependent_slices() {
    let sps = sps();
    let plain =
        parse_pps::<TestError, TestGate>(&rbsp(&pps_body()), Some(&sps)).expect("valid pps");
    let checked: Result<(), TestError> = StrictGate::check_pps(&plain, Some(&sps));
    assert!(checked.is_ok());
    let dependent = parse_pps::<TestError, TestGate>(&rbsp(&pps_body_opts(true)), Some(&sps))
        .expect("dependent pps");
    assert!(dependent.dependent_slices());
    assert_eq!(
        StrictGate::check_pps(&dependent, Some(&sps)),
        Err(TestError::Unsupported(UnsupportedReason::DependentSlices))
    );
}

#[test]
fn strict_gate_orders_slices() {
    let ordered: Result<(), TestError> =
        StrictGate::check_slice_order(&[0, 5, 9], &[true, false, false]);
    assert!(ordered.is_ok());
    assert_eq!(
        StrictGate::check_slice_order(&[5, 5], &[true, false]),
        Err(TestError::Unsupported(
            UnsupportedReason::ArbitrarySliceOrder
        ))
    );
}

#[test]
fn pps_parses_explicit_tiles() {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 3);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.se(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.ue(1);
    writer.ue(0);
    writer.bit(false);
    writer.ue(1);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    let pps =
        parse_pps::<TestError, TestGate>(&rbsp(&writer.finish()), Some(&sps())).expect("tiled pps");
    assert!(pps.tiles_enabled());
    assert!(!pps.uniform_spacing());
    assert!(pps.loop_across_tiles());
    assert_eq!(pps.column_width, vec![2, 3]);
    assert_eq!(pps.row_height, vec![4]);
}

#[test]
fn slice_idr_parses() {
    let mut ingest = Ingest::<TestError, TestGate>::new();
    let nals = param_nals();
    let mut all = nals;
    all.push(nal(20, 0, &idr_slice_body()));
    let pictures = ingest.ingest_packet(&packet(&all)).expect("idr packet");
    assert_eq!(pictures.len(), 1);
    let picture = &pictures[0];
    assert!(picture.idr);
    assert!(picture.irap);
    assert!(!picture.rasl);
    assert_eq!(picture.slices.len(), 1);
    let header = &picture.slices[0].header;
    assert!(header.first);
    assert_eq!(header.slice_type, SliceType::I);
    assert_eq!(header.poc_lsb, None);
    assert!(header.rps.is_none());
    assert!(header.pic_output);
    assert!(!header.no_output_prior);
}

#[test]
fn slice_p_parses_with_table_rps() {
    let mut ingest = Ingest::<TestError, TestGate>::new();
    let mut all = param_nals();
    all.push(nal(1, 0, &p_slice_body(3)));
    let pictures = ingest.ingest_packet(&packet(&all)).expect("p packet");
    assert_eq!(pictures.len(), 1);
    let header = &pictures[0].slices[0].header;
    assert_eq!(header.slice_type, SliceType::P);
    assert_eq!(header.poc_lsb, Some(3));
    let rps = header.rps.as_ref().expect("table rps");
    assert_eq!(rps.negative[0], -1);
    let bits = header.rps_bits.expect("rps provenance");
    assert!(!bits.explicit);
    assert!(!pictures[0].idr);
}

#[test]
fn slice_b_parses_explicit_rps() {
    let mut ingest = Ingest::<TestError, TestGate>::new();
    let mut all = param_nals();
    all.push(nal(1, 0, &b_slice_body(2)));
    let pictures = ingest.ingest_packet(&packet(&all)).expect("b packet");
    assert_eq!(pictures.len(), 1);
    let header = &pictures[0].slices[0].header;
    assert_eq!(header.slice_type, SliceType::B);
    assert_eq!(header.poc_lsb, Some(2));
    let rps = header.rps.as_ref().expect("explicit rps");
    assert_eq!(rps.negative[0], -2);
    assert!(rps.negative_used[0]);
    assert_eq!(rps.positive[0], 1);
    assert!(!rps.positive_used[0]);
    let bits = header.rps_bits.expect("rps provenance");
    assert!(bits.explicit);
    assert!(bits.bits > 0);
}

#[test]
fn ingest_packet_groups_pictures() {
    let mut ingest = Ingest::<TestError, TestGate>::new();
    let mut all = param_nals();
    all.push(nal(20, 0, &idr_slice_body()));
    all.push(nal(1, 0, &p_slice_body(1)));
    let pictures = ingest.ingest_packet(&packet(&all)).expect("two pictures");
    assert_eq!(pictures.len(), 2);
    assert!(pictures[0].idr);
    assert_eq!(pictures[1].poc_lsb(), Some(1));
    assert_eq!(pictures[1].pps_id(), 0);
}

#[test]
fn ingest_drops_stream_start_rasl() {
    let mut ingest = Ingest::<TestError, TestGate>::new();
    let mut all = param_nals();
    all.push(nal(21, 0, &cra_slice_body(8)));
    all.push(nal(8, 0, &p_slice_body(7)));
    let pictures = ingest.ingest_packet(&packet(&all)).expect("cra packet");
    assert_eq!(pictures.len(), 1);
    assert!(pictures[0].irap);

    let next = [nal(21, 0, &cra_slice_body(8)), nal(8, 0, &p_slice_body(7))];
    let pictures = ingest.ingest_packet(&packet(&next)).expect("midstream cra");
    assert_eq!(pictures.len(), 2);
    assert!(pictures[1].rasl);
}

#[test]
fn ingest_declines_dependent_and_multilayer() {
    let mut ingest = Ingest::<TestError, TestGate>::new();
    let mut second = Bits::new();
    second.bit(false);
    second.bit(true);
    let all = vec![
        nal(32, 0, &[0x00, 0x80]),
        nal(33, 0, &sps_body()),
        nal(34, 0, &pps_body_opts(true)),
        nal(1, 0, &p_slice_body(1)),
        nal(1, 0, &second.finish()),
    ];
    let error = ingest.ingest_packet(&packet(&all)).unwrap_err();
    assert!(matches!(error, TestError::Unsupported(_)));

    let mut ingest = Ingest::<TestError, TestGate>::new();
    let error = ingest
        .ingest_packet(&packet(&[vec![0x02, 0x41, 0x80]]))
        .unwrap_err();
    assert!(matches!(error, TestError::Unsupported(_)));
}

#[test]
fn ingest_rejects_unknown_sets() {
    let mut ingest = Ingest::<TestError, TestGate>::new();
    let error = ingest
        .ingest_packet(&packet(&[nal(33, 0, &sps_body())]))
        .unwrap_err();
    assert_eq!(error, TestError::SpsCitesUnknownVps);

    let mut ingest = Ingest::<TestError, TestGate>::new();
    let mut all = param_nals();
    let mut bad = Bits::new();
    bad.bit(true);
    bad.bit(false);
    bad.ue(3);
    all.push(nal(20, 0, &bad.finish()));
    let error = ingest.ingest_packet(&packet(&all)).unwrap_err();
    assert!(matches!(error, TestError::Parse(_)));
}

#[test]
fn scaling_list_data_parses_modes() {
    let mut writer = Bits::new();
    for size_id in 0..4 {
        let matrices = if size_id == 3 { 2 } else { 6 };
        for matrix_id in 0..matrices {
            if matrix_id == 0 {
                writer.bit(true);
                if size_id > 1 {
                    writer.se(0);
                }
                let count = 1usize << (4 + (size_id << 1).min(2));
                for _ in 0..count.min(64) {
                    writer.se(0);
                }
            } else {
                writer.bit(false);
                writer.ue(0);
            }
        }
    }
    let lists = parse_scaling_list_data::<TestError>(&mut BitReader::new(&writer.finish_rbsp()))
        .expect("lists");
    assert_eq!(lists.size0[0], [8; 16]);
    assert_eq!(lists.size0[1], [16; 16]);
    assert_eq!(lists.size1[0][0], 8);
    assert_eq!(lists.size1[0][63], 8);
    assert_eq!(lists.size1[1][63], 115);
    assert_eq!(lists.size1[4][63], 91);
    assert_eq!(lists.dc2[0], 8);
    assert_eq!(lists.dc2[1], 16);
    assert_eq!(lists.dc3, [8, 16]);
}

#[test]
fn conformance_crop_insets_visible() {
    let mut writer = Bits::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    ptl(&mut writer, false);
    writer.ue(0);
    writer.ue(1);
    writer.ue(320);
    writer.ue(248);
    writer.bit(true);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(4);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.ue(4);
    writer.ue(2);
    writer.ue(5);
    writer.ue(0);
    writer.ue(3);
    writer.ue(0);
    writer.ue(3);
    writer.ue(1);
    writer.ue(1);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    let sps = parse_sps::<TestError, TestGate>(&rbsp(&writer.finish())).expect("cropped sps");
    assert_eq!(sps.visible_width(), 320);
    assert_eq!(sps.visible_height(), 240);
}

#[test]
fn nal_layer_id_reads_six_bits() {
    let mut ingest = Ingest::<TestError, LayerGate>::new();
    let packet = [0, 0, 1, 0x02, 0x11, 0xAA];
    ingest.ingest_packet(&packet).expect("layer-2 NAL ingests");
}

#[test]
fn poc_from_lsb_wraps_against_anchor() {
    let sps = Sps {
        log2_max_poc_lsb_minus4: 4,
        ..sps()
    };
    assert_eq!(sps.poc_from_lsb(None, 200, false), Some(200));
    assert_eq!(sps.poc_from_lsb(Some(0), 200, false), Some(-56));
    assert_eq!(sps.poc_from_lsb(Some(300), 200, false), Some(200));
    assert_eq!(sps.poc_from_lsb(Some(300), 50, false), Some(306));
    assert_eq!(sps.poc_from_lsb(Some(300), 50, true), Some(50));
}

#[test]
fn lt_msb_present_adds_cycle_and_lsb() {
    use crate::slice::LtSpec;

    let spec = LtSpec {
        poc_lsb: 10,
        used: true,
        msb_present: true,
        msb_cycle: 1,
    };
    assert_eq!(spec.poc_msb_present(300, 256), Some(10));
    assert_eq!(spec.poc_msb_present(44, 256), Some(-246));
    let spec = LtSpec {
        msb_cycle: 0,
        ..spec
    };
    assert_eq!(spec.poc_msb_present(300, 256), Some(266));
}

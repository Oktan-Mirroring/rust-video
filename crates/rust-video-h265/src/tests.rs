//! Grammar tests behind synthetic bitstreams.

use crate::bits::BitReader;
use crate::error::test_support::TestError;
use crate::error::HevcGate;
use crate::error::NalAction;
use crate::nal::rbsp;
use crate::picture::Ingest;
use crate::pps::{parse_pps, Pps};
use crate::scaling::parse_scaling_list_data;
use crate::slice::SliceType;
use crate::sps::{parse_sps, Sps};

/// Test gate with ingest citation checks.
struct TestGate;

impl HevcGate<TestError> for TestGate {
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
struct LayerGate;

impl HevcGate<TestError> for LayerGate {
    /// Layer 2 expected; anything else fails the pin.
    fn classify_nal(nal_type: u8, layer_id: u16) -> Result<NalAction, TestError> {
        assert_eq!((nal_type, layer_id), (1, 2));
        Ok(NalAction::Ignore)
    }

    /// Unused: layer pin never stores sets.
    fn check_sps(sps: &Sps) -> Result<(), TestError> {
        let _ = sps;
        Ok(())
    }

    /// Unused: layer pin never stores sets.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), TestError> {
        let _ = (pps, sps);
        Ok(())
    }
}

/// Test bit writer with emulation prevention.
struct BitWriter {
    /// Finished bytes.
    bytes: Vec<u8>,
    /// Open byte under fill.
    current: u8,
    /// Bits filled in the open byte.
    filled: u8,
}

impl BitWriter {
    /// Empty writer.
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            filled: 0,
        }
    }

    /// Single bit.
    fn bit(&mut self, set: bool) {
        if set {
            self.current |= 1 << (7 - self.filled);
        }
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.filled = 0;
        }
    }

    /// Low `count` bits of `value`, most significant first.
    fn bits(&mut self, value: u32, count: u32) {
        for index in (0..count).rev() {
            self.bit(value >> index & 1 != 0);
        }
    }

    /// Unsigned Exp-Golomb code.
    fn ue(&mut self, value: u32) {
        let code = value + 1;
        let width = 32 - code.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        self.bits(code, width);
    }

    /// Signed Exp-Golomb code.
    fn se(&mut self, value: i32) {
        let code = if value > 0 {
            u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
        } else {
            u32::try_from(-value).unwrap_or(u32::MAX) * 2
        };
        self.ue(code);
    }

    /// Profile-tier-level with Main profile compatibility.
    fn ptl(&mut self, interlaced: bool) {
        self.bits(0, 2);
        self.bit(false);
        self.bits(1, 5);
        for index in 0..32 {
            self.bit(index == 1);
        }
        self.bit(!interlaced);
        self.bit(interlaced);
        self.bit(true);
        self.bit(true);
        self.bits(0, 24);
        self.bits(0, 19);
        self.bit(false);
        self.bits(120, 8);
    }

    /// Stop bit plus byte alignment.
    fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.filled != 0 {
            self.bit(false);
        }
        self.bytes
    }

    /// Finished RBSP with emulation prevention inserted.
    fn finish_ebsp(self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut zeros = 0;
        for byte in self.finish() {
            if zeros == 2 && byte <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        out
    }
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
    let mut writer = BitWriter::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    writer.ptl(false);
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
    writer.finish_ebsp()
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
    let mut writer = BitWriter::new();
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
    writer.finish_ebsp()
}

/// IDR slice header body over the test sets.
fn idr_slice_body() -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(2);
    writer.bit(true);
    writer.bit(true);
    writer.se(0);
    writer.bit(true);
    writer.ue(0);
    writer.finish_ebsp()
}

/// P slice header body with `lsb` over the test sets.
fn p_slice_body(lsb: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
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
    writer.finish_ebsp()
}

/// B slice header body with an explicit reference set.
fn b_slice_body(lsb: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
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
    writer.finish_ebsp()
}

/// CRA I slice header body with `lsb` over the test sets.
fn cra_slice_body(lsb: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
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
    writer.finish_ebsp()
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
    let mut writer = BitWriter::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    writer.ptl(false);
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
    let sps =
        parse_sps::<TestError, TestGate>(&rbsp(&writer.finish_ebsp())).expect("predicted rps");
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
fn pps_parses_explicit_tiles() {
    let mut writer = BitWriter::new();
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
    let pps = parse_pps::<TestError, TestGate>(&rbsp(&writer.finish_ebsp()), Some(&sps()))
        .expect("tiled pps");
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
    let mut second = BitWriter::new();
    second.bit(false);
    second.bit(true);
    let all = vec![
        nal(32, 0, &[0x00, 0x80]),
        nal(33, 0, &sps_body()),
        nal(34, 0, &pps_body_opts(true)),
        nal(1, 0, &p_slice_body(1)),
        nal(1, 0, &second.finish_ebsp()),
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
    let mut bad = BitWriter::new();
    bad.bit(true);
    bad.bit(false);
    bad.ue(3);
    all.push(nal(20, 0, &bad.finish_ebsp()));
    let error = ingest.ingest_packet(&packet(&all)).unwrap_err();
    assert!(matches!(error, TestError::Parse(_)));
}

#[test]
fn scaling_list_data_parses_modes() {
    let mut writer = BitWriter::new();
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
    let lists =
        parse_scaling_list_data::<TestError>(&mut BitReader::new(&writer.finish())).expect("lists");
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
    let mut writer = BitWriter::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    writer.ptl(false);
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
    let sps = parse_sps::<TestError, TestGate>(&rbsp(&writer.finish_ebsp())).expect("cropped sps");
    assert_eq!(sps.visible_width(), 320);
    assert_eq!(sps.visible_height(), 240);
}

#[test]
fn nal_layer_id_reads_six_bits() {
    let mut ingest = Ingest::<TestError, LayerGate>::new();
    let packet = [0, 0, 1, 0x02, 0x11, 0xAA];
    ingest.ingest_packet(&packet).expect("layer-2 NAL ingests");
}

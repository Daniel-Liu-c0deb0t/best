use noodles::bam;
use noodles::core::Position;
use noodles::fasta;
use noodles::sam;

use sam::record::cigar::op::Kind;
use sam::record::data::field::Tag;

use fxhash::FxHashMap;

use crate::bed::*;

/// Statistics for each alignment.
#[derive(Debug)]
pub struct AlnStats<'a> {
    pub read_name: String,
    pub q_len: usize,
    effective_cov: Option<f32>,
    subread_passes: Option<usize>,
    pred_concordance: Option<f32>,
    pub supplementary: bool,
    mapq: u8,
    mean_qual: u8,
    pub read_len: usize,
    pub concordance: f32,
    pub concordance_gc: f32,
    pub concordance_qv: f32,
    pub matches: usize,
    pub mismatches: usize,
    pub non_hp_ins: usize,
    pub non_hp_del: usize,
    pub hp_ins: usize,
    pub hp_del: usize,
    pub gc_ins: usize,
    pub gc_del: usize,
    pub feature_stats: FxHashMap<&'a str, FeatureStats>,
}

/// Statistics for each interval feature.
#[derive(Debug, Default)]
pub struct FeatureStats {
    pub overlaps: usize,
    pub matches: usize,
    pub mismatches: usize,
    pub non_hp_ins: usize,
    pub non_hp_del: usize,
    pub hp_ins: usize,
    pub hp_del: usize,
    pub gc_ins: usize,
    pub gc_del: usize,
}

impl FeatureStats {
    pub fn assign_add(&mut self, o: &Self) {
        self.overlaps += o.overlaps;
        self.matches += o.matches;
        self.mismatches += o.mismatches;
        self.non_hp_ins += o.non_hp_ins;
        self.non_hp_del += o.non_hp_del;
        self.hp_ins += o.hp_ins;
        self.hp_del += o.hp_del;
        self.gc_ins += o.gc_ins;
        self.gc_del += o.gc_del;
    }

    pub fn num_bases(&self) -> usize {
        self.matches + self.mismatches + self.non_hp_del + self.hp_del
    }
}

impl<'a> AlnStats<'a> {
    pub fn from_record(
        references: &sam::header::ReferenceSequences,
        reference_seqs: &FxHashMap<String, fasta::Record>,
        r: &bam::lazy::Record,
        intervals: &[&'a FeatureInterval],
    ) -> Option<Self> {
        let flags = r.flags().ok()?;
        if flags.is_unmapped() || flags.is_secondary() {
            // skip
            return None;
        }

        // note: avoid copying data (especially sequence/quality scores) since they are large
        let sequence = r.sequence().ok()?;
        let q_scores = r.quality_scores().ok()?;
        let data = r.data().ok()?;
        let ec_tag = Tag::try_from(*b"ec").ok()?;
        let ec = data.get(ec_tag).map(|f| f.value().as_float().unwrap());
        let np_tag: Tag = Tag::try_from(*b"np").ok()?;
        let np = data
            .get(np_tag)
            .map(|f| f.value().as_int().unwrap() as usize);
        let rq_tag: Tag = Tag::try_from(*b"rq").ok()?;
        let rq = data.get(rq_tag).map(|f| f.value().as_float().unwrap());

        let mut res = AlnStats {
            read_name: r.read_name().ok()??.to_string(),
            q_len: sequence.len(),
            effective_cov: ec,
            subread_passes: np,
            pred_concordance: rq,
            supplementary: flags.is_supplementary(),
            mapq: u8::from(r.mapping_quality().ok()??),
            mean_qual: mean_qual(q_scores.as_ref()),
            // fill in the rest afterwards
            read_len: 0,
            concordance: 0.0,
            concordance_gc: 0.0,
            concordance_qv: 0.0,
            matches: 0,
            mismatches: 0,
            non_hp_ins: 0,
            non_hp_del: 0,
            hp_ins: 0,
            hp_del: 0,
            gc_ins: 0,
            gc_del: 0,
            feature_stats: FxHashMap::default(),
        };

        for i in intervals {
            res.feature_stats
                .entry(&i.val)
                .or_insert_with(|| FeatureStats::default())
                .overlaps += 1;
        }

        let mut ref_pos = usize::from(r.alignment_start().ok()??);
        let mut query_pos = 1;
        let mut interval_idx = 0;
        let curr_ref_name = references[r.reference_sequence_id().ok()??]
            .name()
            .to_string();
        let curr_ref_seq = reference_seqs[&curr_ref_name].sequence();

        // count mismatches, indels, and homopolymers
        for op in r.cigar().ok()?.iter() {
            for _i in 0..op.len() {
                while interval_idx < intervals.len() && ref_pos >= intervals[interval_idx].stop {
                    interval_idx += 1;
                }
                let in_interval =
                    interval_idx < intervals.len() && ref_pos >= intervals[interval_idx].start;
                let curr_feature = if in_interval {
                    Some(
                        res.feature_stats
                            .get_mut(intervals[interval_idx].val.as_str())
                            .unwrap(),
                    )
                } else {
                    None
                };

                match op.kind() {
                    Kind::SequenceMatch => {
                        res.matches += 1;
                        if in_interval {
                            curr_feature.unwrap().matches += 1;
                        }
                        query_pos += 1;
                        ref_pos += 1;
                    }
                    Kind::SequenceMismatch => {
                        res.mismatches += 1;
                        if in_interval {
                            curr_feature.unwrap().mismatches += 1;
                        }
                        query_pos += 1;
                        ref_pos += 1;
                    }
                    Kind::Insertion => {
                        // can be computed without looping through the number of insertions
                        // this does not modify ref_pos
                        let before_ins = curr_ref_seq[Position::new(ref_pos)?].to_ascii_uppercase();
                        let after_ins = curr_ref_seq
                            .get(Position::new(ref_pos + 1)?)
                            .unwrap_or(&b'?')
                            .to_ascii_uppercase();
                        let query_ins = &sequence
                            [Position::new(query_pos)?..Position::new(query_pos + op.len())?];
                        let hp_before = query_ins
                            .iter()
                            .map(|&c| u8::from(c).to_ascii_uppercase())
                            .all(|c| c == before_ins);
                        let hp_after = query_ins
                            .iter()
                            .map(|&c| u8::from(c).to_ascii_uppercase())
                            .all(|c| c == after_ins);
                        if hp_before || hp_after {
                            res.hp_ins += op.len();
                            if in_interval {
                                curr_feature.unwrap().hp_ins += op.len();
                            }
                        } else {
                            res.non_hp_ins += op.len();
                            if in_interval {
                                curr_feature.unwrap().non_hp_ins += op.len();
                            }
                        }
                        query_pos += op.len();
                        break;
                    }
                    Kind::Deletion => {
                        let before_curr = curr_ref_seq
                            .get(Position::new(ref_pos - 1)?)
                            .unwrap_or(&b'?')
                            .to_ascii_uppercase();
                        let after_curr = curr_ref_seq
                            .get(Position::new(ref_pos + 1)?)
                            .unwrap_or(&b'?')
                            .to_ascii_uppercase();
                        let curr = curr_ref_seq[Position::new(ref_pos)?].to_ascii_uppercase();
                        let hp = curr == before_curr || curr == after_curr;
                        if hp {
                            res.hp_del += 1;
                            if in_interval {
                                curr_feature.unwrap().hp_del += 1;
                            }
                        } else {
                            res.non_hp_del += 1;
                            if in_interval {
                                curr_feature.unwrap().non_hp_del += 1;
                            }
                        }
                        ref_pos += 1;
                    }
                    Kind::SoftClip => {
                        // does not require looping through the number of soft clips
                        query_pos += op.len();
                        break;
                    }
                    _ => panic!("Unexpected CIGAR operation!"),
                }
            }

            // gap compressed
            match op.kind() {
                Kind::Insertion => res.gc_ins += 1,
                Kind::Deletion => res.gc_del += 1,
                _ => (),
            }
        }

        let errors = res.mismatches + res.non_hp_ins + res.non_hp_del + res.hp_ins + res.hp_del;
        res.read_len = res.matches + res.mismatches + res.non_hp_del + res.hp_del;
        res.concordance = (res.matches as f32) / ((res.matches + errors) as f32);
        res.concordance_gc = (res.matches as f32)
            / ((res.matches + res.mismatches + res.gc_ins + res.gc_del) as f32);
        res.concordance_qv = concordance_qv(res.concordance, errors > 0);

        Some(res)
    }

    pub fn header() -> &'static str {
        "read,readLengthBp,effectiveCoverage,subreadPasses,predictedConcordance,alignmentType,alignmentMapq,meanQuality,hcReadLengthBp,concordance,concordanceGc,concordanceQv,mismatchBp,nonHpInsertionBp,nonHpDeletionBp,hpInsertionBp,hpDeletionBp"
    }

    pub fn to_csv(&self) -> String {
        let supp_str = if self.supplementary {
            "Supplementary"
        } else {
            "Primary"
        };
        let ec = self
            .effective_cov
            .map(|x| format!("{:.2}", x))
            .unwrap_or_else(|| String::new());
        let np = self
            .subread_passes
            .map(|x| format!("{}", x))
            .unwrap_or_else(|| String::new());
        let rq = self
            .pred_concordance
            .map(|x| format!("{:.6}", x))
            .unwrap_or_else(|| String::new());
        format!(
            "{},{},{:.2},{},{:.6},{},{},{},{},{:.6},{:.6},{:.2},{},{},{},{},{}",
            self.read_name,
            self.q_len,
            ec,
            np,
            rq,
            supp_str,
            self.mapq,
            self.mean_qual,
            self.read_len,
            self.concordance,
            self.concordance_gc,
            self.concordance_qv,
            self.mismatches,
            self.non_hp_ins,
            self.non_hp_del,
            self.hp_ins,
            self.hp_del
        )
    }
}

fn concordance_qv(concordance: f32, has_errors: bool) -> f32 {
    if has_errors {
        -10.0f32 * (1.0f32 - concordance).log10()
    } else {
        60.0f32
    }
}

fn mean_qual(q_scores: &[sam::record::quality_scores::Score]) -> u8 {
    let sum_q = q_scores
        .iter()
        .map(|&q| 10.0f32.powf(-(u8::from(q) as f32) / 10.0f32))
        .sum::<f32>();
    (-10.0f32 * (sum_q / (q_scores.len() as f32)).log10()).round() as u8
}

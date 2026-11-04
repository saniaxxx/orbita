//! Vosk TTS on ONNX Runtime: BERT word embeddings and the multistream VITS
//! synthesizer. Inputs mirror `Synth.synth_audio` for `multistream_v1`.
//!
//! Both sessions can sit on different devices (`VOSK_BERT_DEVICE`): the BERT
//! export is uint8-quantised and its integer ops may fall back to the CPU, so
//! keeping it off the GPU is sometimes faster and saves VRAM.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use ort::{
    execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch},
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};

use super::{
    Lexicon, VoskConfig,
    g2p::{G2pOutput, multistream},
    tokenizer::word_rows,
};

const BERT_DIM: usize = 768;

/// Noise and tempo knobs of one synthesis call (`scales` input).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scales {
    pub noise: f32,
    pub length: f32,
    pub duration_noise: f32,
}

/// Where one piece of text spent its time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PieceTimings {
    pub tokens: usize,
    pub phonemes: usize,
    pub bert: Duration,
    pub g2p: Duration,
    pub synth: Duration,
}

pub struct VoskModel {
    pub lexicon: Lexicon,
    bert: Session,
    synthesizer: Session,
}

impl VoskModel {
    pub fn load(config: &VoskConfig, lexicon: Lexicon) -> Result<Self, String> {
        let open = |path: &Path, cuda: bool| -> Result<Session, String> {
            let providers = execution_providers(config, cuda)?;
            Session::builder()
                .and_then(|builder| builder.with_optimization_level(GraphOptimizationLevel::Level3))
                .and_then(|builder| builder.with_intra_threads(config.threads))
                .and_then(|builder| builder.with_execution_providers(providers))
                .and_then(|builder| builder.commit_from_file(path))
                .map_err(|error| format!("loading {}: {error}", path.display()))
        };
        Ok(Self {
            bert: open(&config.files.bert, config.bert_cuda)?,
            synthesizer: open(&config.files.synthesizer, config.cuda)?,
            lexicon,
        })
    }

    /// Selected BERT rows (`get_word_bert(nopunc=True)`), row-major `[rows, 768]`.
    pub fn word_embeddings(&mut self, text: &str) -> Result<(Vec<f32>, usize), String> {
        let (embeddings, rows, _) = self.word_embeddings_timed(text)?;
        Ok((embeddings, rows))
    }

    fn word_embeddings_timed(&mut self, text: &str) -> Result<(Vec<f32>, usize, usize), String> {
        let encoding = self.lexicon.tokenizer.encode(&text.replace('+', ""));
        let length = encoding.ids.len();
        let ones = vec![1i64; length];
        let zeros = vec![0i64; length];
        let outputs = self
            .bert
            .run(ort::inputs![
                "input_ids" => tensor([1usize, length], encoding.ids.clone())?,
                "attention_mask" => tensor([1usize, length], ones)?,
                "token_type_ids" => tensor([1usize, length], zeros)?,
            ])
            .map_err(ort_error)?;
        let (shape, data) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(ort_error)?;
        // The export squeezes the batch: [tokens, 768] (tolerate [1, tokens, 768]).
        let dims: Vec<i64> = shape.iter().copied().collect();
        let width = *dims.last().unwrap_or(&0) as usize;
        if width != BERT_DIM || data.len() != length * BERT_DIM {
            return Err(format!("unexpected BERT output shape {dims:?}"));
        }
        let rows = word_rows(&encoding.tokens);
        let mut selected = Vec::with_capacity(rows.len() * BERT_DIM);
        for row in &rows {
            selected.extend_from_slice(&data[row * BERT_DIM..(row + 1) * BERT_DIM]);
        }
        Ok((selected, rows.len(), length))
    }

    pub fn phonemes(&self, text: &str, bert_rows: usize) -> G2pOutput {
        multistream(
            text,
            &self.lexicon.dictionary,
            &self.lexicon.phoneme_ids,
            bert_rows,
        )
    }

    /// Synthesize one piece of prepared text; returns float samples at 22.05 kHz.
    pub fn synthesize(
        &mut self,
        text: &str,
        speaker: i64,
        scales: Scales,
    ) -> Result<Vec<f32>, String> {
        self.synthesize_timed(text, speaker, scales)
            .map(|(audio, _)| audio)
    }

    /// [`Self::synthesize`] with the time spent in each stage.
    pub fn synthesize_timed(
        &mut self,
        text: &str,
        speaker: i64,
        scales: Scales,
    ) -> Result<(Vec<f32>, PieceTimings), String> {
        let mut timings = PieceTimings::default();

        let started = Instant::now();
        let (embeddings, rows, tokens) = self.word_embeddings_timed(text)?;
        timings.bert = started.elapsed();
        timings.tokens = tokens;
        if rows == 0 {
            return Ok((Vec::new(), timings));
        }

        let started = Instant::now();
        let g2p = self.phonemes(text, rows);
        timings.g2p = started.elapsed();
        if !g2p.dropped.is_empty() {
            tracing::warn!(dropped = ?g2p.dropped, "phonemes missing from the model table were skipped");
        }
        let time = g2p.ids.len();
        timings.phonemes = time;

        // input: [1, 5, time]; bert: [1, 768, time]
        let mut streams = vec![0i64; 5 * time];
        for (t, ids) in g2p.ids.iter().enumerate() {
            for (stream, id) in ids.iter().enumerate() {
                streams[stream * time + t] = *id;
            }
        }
        let mut bert = vec![0f32; BERT_DIM * time];
        for (t, row) in g2p.bert_index.iter().enumerate() {
            let source = &embeddings[row * BERT_DIM..(row + 1) * BERT_DIM];
            for (dim, value) in source.iter().enumerate() {
                bert[dim * time + t] = *value;
            }
        }

        let started = Instant::now();
        let outputs = self
            .synthesizer
            .run(ort::inputs![
                "input" => tensor([1usize, 5, time], streams)?,
                "input_lengths" => tensor([1usize], vec![time as i64])?,
                "scales" => tensor([3usize], vec![scales.noise, scales.length, scales.duration_noise])?,
                "sid" => tensor([1usize], vec![speaker])?,
                "bert" => tensor([1usize, BERT_DIM, time], bert)?,
            ])
            .map_err(ort_error)?;
        let (_, wav) = outputs["wav"]
            .try_extract_tensor::<f32>()
            .map_err(ort_error)?;
        let audio = wav.to_vec();
        timings.synth = started.elapsed();
        Ok((audio, timings))
    }
}

fn tensor<T, const N: usize>(shape: [usize; N], data: Vec<T>) -> Result<Tensor<T>, String>
where
    T: ort::tensor::PrimitiveTensorElementType + std::fmt::Debug + Clone + 'static,
{
    Tensor::from_array((shape, data)).map_err(ort_error)
}

fn ort_error(error: ort::Error) -> String {
    error.to_string()
}

fn execution_providers(
    config: &VoskConfig,
    cuda: bool,
) -> Result<Vec<ExecutionProviderDispatch>, String> {
    if cuda {
        cuda_providers(config)
    } else {
        Ok(vec![CPUExecutionProvider::default().build()])
    }
}

#[cfg(feature = "cuda")]
fn cuda_providers(config: &VoskConfig) -> Result<Vec<ExecutionProviderDispatch>, String> {
    use ort::execution_providers::{
        ArenaExtendStrategy,
        cuda::{CUDAExecutionProvider, CuDNNConvAlgorithmSearch},
    };

    let mut provider = CUDAExecutionProvider::default();
    if let Some(search) = config.cudnn_search {
        provider = provider.with_conv_algorithm_search(match search {
            super::CudnnSearch::Heuristic => CuDNNConvAlgorithmSearch::Heuristic,
            super::CudnnSearch::Exhaustive => CuDNNConvAlgorithmSearch::Exhaustive,
            super::CudnnSearch::Default => CuDNNConvAlgorithmSearch::Default,
        });
    }
    if let Some(tf32) = config.tf32 {
        provider = provider.with_tf32(tf32);
    }
    if let Some(arena) = config.arena {
        provider = provider.with_arena_extend_strategy(if arena {
            ArenaExtendStrategy::SameAsRequested
        } else {
            ArenaExtendStrategy::NextPowerOfTwo
        });
    }
    Ok(vec![provider.build().error_on_failure()])
}

#[cfg(not(feature = "cuda"))]
fn cuda_providers(_config: &VoskConfig) -> Result<Vec<ExecutionProviderDispatch>, String> {
    Err("PROVIDERS=cuda needs a build with the `cuda` feature".to_owned())
}

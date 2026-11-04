//! GigaAM v3 on ONNX Runtime.
//!
//! Model files are the onnx-asr exports (`istupakov/gigaam-v3-onnx`, MIT):
//! `<model>_encoder[.int8].onnx`, `<model>_decoder…`, `<model>_joint…` and
//! `<model>_vocab.txt` for RNN-T, `<model>[.int8].onnx` for CTC. They are not
//! part of the repository; `GIGAAM_MODEL_DIR` points at them.

use std::path::{Path, PathBuf};

use ort::{
    execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch},
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};

use super::{
    GigaamConfig, Head,
    decoding::{DecoderState, Transducer, Vocab, ctc_greedy, rnnt_greedy},
    features::{LogMel, N_MELS},
};

/// Prediction-network width of GigaAM v3 RNN-T heads.
const PRED_HIDDEN: usize = 320;
/// Encoder time reduction (`subsampling_factor` in the model config).
const SUBSAMPLING: usize = 4;
const MAX_TOKENS_PER_STEP: usize = 3;

pub struct GigaamEngine {
    features: LogMel,
    vocab: Vocab,
    head: HeadSessions,
    providers: Vec<String>,
}

enum HeadSessions {
    Rnnt {
        encoder: Session,
        prediction: RnntPrediction,
    },
    Ctc {
        model: Session,
    },
}

struct RnntPrediction {
    decoder: Session,
    joint: Session,
}

impl GigaamEngine {
    pub fn load(config: &GigaamConfig) -> Result<Self, String> {
        let dir = Path::new(&config.model_dir);
        let suffix = config.quantization.file_suffix();
        let vocab_path = vocab_path(dir, &config.model)?;
        let vocab = Vocab::parse(
            &std::fs::read_to_string(&vocab_path)
                .map_err(|error| format!("reading {}: {error}", vocab_path.display()))?,
        )?;

        let providers = execution_providers(config)?;
        let open = |name: String| -> Result<Session, String> {
            let path = dir.join(&name);
            if !path.is_file() {
                return Err(format!("model file {} not found", path.display()));
            }
            Session::builder()
                .and_then(|builder| builder.with_optimization_level(GraphOptimizationLevel::Level3))
                .and_then(|builder| builder.with_intra_threads(config.threads))
                .and_then(|builder| builder.with_execution_providers(providers.clone()))
                .and_then(|builder| builder.commit_from_file(&path))
                .map_err(|error| format!("loading {}: {error}", path.display()))
        };

        let head = match config.head() {
            Head::Rnnt => HeadSessions::Rnnt {
                encoder: open(format!("{}_encoder{suffix}.onnx", config.model))?,
                prediction: RnntPrediction {
                    decoder: open(format!("{}_decoder{suffix}.onnx", config.model))?,
                    joint: open(format!("{}_joint{suffix}.onnx", config.model))?,
                },
            },
            Head::Ctc => HeadSessions::Ctc {
                model: open(format!("{}{suffix}.onnx", config.model))?,
            },
        };

        Ok(Self {
            features: LogMel::new(),
            vocab,
            head,
            providers: config.provider_names(),
        })
    }

    pub fn providers(&self) -> &[String] {
        &self.providers
    }

    /// Recognise one utterance of 16 kHz mono samples in `[-1, 1]`.
    pub fn recognize(&mut self, samples: &[f32]) -> Result<String, String> {
        let features = self.features.compute(samples);
        if features.frames == 0 {
            return Ok(String::new());
        }
        let shape = [1usize, N_MELS, features.frames];
        let lengths = [features.frames as i64];

        let tokens = match &mut self.head {
            HeadSessions::Rnnt {
                encoder,
                prediction,
            } => {
                let outputs = encoder
                    .run(ort::inputs![
                        "audio_signal" => tensor(shape, features.data)?,
                        "length" => tensor([1usize], lengths.to_vec())?,
                    ])
                    .map_err(ort_error)?;
                let (encoded_shape, encoded) = outputs["encoded"]
                    .try_extract_tensor::<f32>()
                    .map_err(ort_error)?;
                let (_, encoded_len) = outputs["encoded_len"]
                    .try_extract_tensor::<i32>()
                    .map_err(ort_error)?;

                // encoded: [1, hidden, time]; gather one frame at a time.
                let hidden = encoded_shape[1] as usize;
                let time = encoded_shape[2] as usize;
                let frames = (encoded_len[0].max(0) as usize).min(time);
                let transposed = transpose(encoded, hidden, time);
                rnnt_greedy(
                    prediction,
                    frames,
                    |t| &transposed[t * hidden..(t + 1) * hidden],
                    self.vocab.blank(),
                    MAX_TOKENS_PER_STEP,
                )?
            }
            HeadSessions::Ctc { model } => {
                let outputs = model
                    .run(ort::inputs![
                        "features" => tensor(shape, features.data)?,
                        "feature_lengths" => tensor([1usize], lengths.to_vec())?,
                    ])
                    .map_err(ort_error)?;
                let (log_probs_shape, log_probs) = outputs["log_probs"]
                    .try_extract_tensor::<f32>()
                    .map_err(ort_error)?;
                let time = log_probs_shape[1] as usize;
                let classes = log_probs_shape[2] as usize;
                let frames = ((features.frames - 1) / SUBSAMPLING + 1).min(time);
                ctc_greedy(log_probs, classes, frames, self.vocab.blank())
            }
        };

        Ok(self.vocab.decode(&tokens).trim().to_owned())
    }
}

impl Transducer for RnntPrediction {
    type Error = String;

    fn predict(
        &mut self,
        token: usize,
        previous: Option<&DecoderState>,
    ) -> Result<DecoderState, String> {
        let zeros = vec![0.0f32; PRED_HIDDEN];
        let (hidden, cell) = match previous {
            Some(state) => (state.hidden.clone(), state.cell.clone()),
            None => (zeros.clone(), zeros),
        };
        let state_shape = [1usize, 1, PRED_HIDDEN];
        let outputs = self
            .decoder
            .run(ort::inputs![
                "x" => tensor([1usize, 1], vec![token as i64])?,
                "h.1" => tensor(state_shape, hidden)?,
                "c.1" => tensor(state_shape, cell)?,
            ])
            .map_err(ort_error)?;
        Ok(DecoderState {
            output: to_vec_f32(&outputs["dec"])?,
            hidden: to_vec_f32(&outputs["h"])?,
            cell: to_vec_f32(&outputs["c"])?,
        })
    }

    fn joint(&mut self, frame: &[f32], decoder: &DecoderState) -> Result<Vec<f32>, String> {
        let outputs = self
            .joint
            .run(ort::inputs![
                "enc" => tensor([1usize, frame.len(), 1], frame.to_vec())?,
                "dec" => tensor([1usize, decoder.output.len(), 1], decoder.output.clone())?,
            ])
            .map_err(ort_error)?;
        to_vec_f32(&outputs["joint"])
    }
}

fn tensor<T, const N: usize>(shape: [usize; N], data: Vec<T>) -> Result<Tensor<T>, String>
where
    T: ort::tensor::PrimitiveTensorElementType + std::fmt::Debug + Clone + 'static,
{
    Tensor::from_array((shape, data)).map_err(ort_error)
}

fn to_vec_f32(value: &ort::value::DynValue) -> Result<Vec<f32>, String> {
    let (_, data) = value.try_extract_tensor::<f32>().map_err(ort_error)?;
    Ok(data.to_vec())
}

/// `[hidden, time]` → `[time, hidden]`.
fn transpose(data: &[f32], hidden: usize, time: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; hidden * time];
    for h in 0..hidden {
        for t in 0..time {
            out[t * hidden + h] = data[h * time + t];
        }
    }
    out
}

fn ort_error(error: ort::Error) -> String {
    error.to_string()
}

fn vocab_path(dir: &Path, model: &str) -> Result<PathBuf, String> {
    // e2e models ship their own vocab; plain v3 heads share v3_vocab.txt.
    let candidates = [
        dir.join(format!("{model}_vocab.txt")),
        dir.join("v3_vocab.txt"),
    ];
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| {
            format!(
                "vocab not found: tried {}",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn execution_providers(config: &GigaamConfig) -> Result<Vec<ExecutionProviderDispatch>, String> {
    match config.device {
        super::Device::Cpu => Ok(vec![CPUExecutionProvider::default().build()]),
        super::Device::Cuda => cuda_providers(config),
    }
}

#[cfg(feature = "cuda")]
fn cuda_providers(config: &GigaamConfig) -> Result<Vec<ExecutionProviderDispatch>, String> {
    use ort::execution_providers::CUDAExecutionProvider;
    Ok(vec![
        CUDAExecutionProvider::default()
            .with_device_id(config.cuda_device)
            .build()
            .error_on_failure(),
    ])
}

#[cfg(not(feature = "cuda"))]
fn cuda_providers(_config: &GigaamConfig) -> Result<Vec<ExecutionProviderDispatch>, String> {
    Err("GIGAAM_DEVICE=cuda needs a build with the `cuda` feature".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transposes_hidden_time() {
        // hidden = 2, time = 3
        let data = [1.0, 2.0, 3.0, 10.0, 20.0, 30.0];
        assert_eq!(
            transpose(&data, 2, 3),
            vec![1.0, 10.0, 2.0, 20.0, 3.0, 30.0]
        );
    }
}

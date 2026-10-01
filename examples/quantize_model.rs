//! Shrinks a model2vec model to int8 for shipping, the way model2vec itself
//! quantizes: every weight scaled by the largest one so that it is 127, then
//! rounded. The scale is not kept, since the model's vectors are normalized
//! and so come out the same whatever the weights are scaled by. The
//! tokenizer and the config go along unchanged.
//!
//! The viewer reads the result as it reads the original, so the flatpak and
//! the macOS bundle run this on the downloaded model rather than ship four
//! bytes per weight.
//! Run: `cargo run --release --example quantize_model -- <source dir> <destination dir>`.

use std::error::Error;
use std::fs;
use std::path::Path;

use safetensors::tensor::{Dtype, TensorView};
use safetensors::{SafeTensors, serialize_to_file};

/// The names model2vec stores the embeddings under, oldest layout last.
const EMBEDDINGS: [&str; 3] = ["embeddings", "0", "embedding.weight"];

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: quantize_model <source dir> <destination dir>";
    let source = args.next().ok_or(usage)?;
    let destination = args.next().ok_or(usage)?;
    let (source, destination) = (Path::new(&source), Path::new(&destination));

    let bytes = fs::read(source.join("model.safetensors"))?;
    let tensors = SafeTensors::deserialize(&bytes)?;
    let (name, embeddings) = EMBEDDINGS
        .iter()
        .find_map(|name| tensors.tensor(name).ok().map(|tensor| (*name, tensor)))
        .ok_or("no embeddings tensor in model.safetensors")?;
    let weights: Vec<f32> = match embeddings.dtype() {
        Dtype::F32 => embeddings
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&bytes| f32::from_le_bytes(bytes))
            .collect(),
        Dtype::I8 => return Err("the model is already int8".into()),
        other => return Err(format!("the embeddings are {other:?}, not f32").into()),
    };
    let largest = weights.iter().fold(0.0_f32, |largest, weight| largest.max(weight.abs()));
    if largest == 0.0 {
        return Err("every weight is zero".into());
    }
    let scale = largest / 127.0;
    let quantized: Vec<u8> = weights
        .iter()
        .map(|weight| (weight / scale).round_ties_even().clamp(-127.0, 127.0) as i8 as u8)
        .collect();
    println!(
        "Quantized {} weights of {:?}, the largest {largest}, from {} to {} bytes.",
        weights.len(),
        embeddings.shape(),
        weights.len() * 4,
        quantized.len()
    );

    // Every other tensor, as a newer model's per-token weights, goes along
    // as it is.
    fs::create_dir_all(destination)?;
    let quantized = TensorView::new(Dtype::I8, embeddings.shape().to_vec(), &quantized)?;
    let mut out: Vec<(String, TensorView)> = vec![(name.to_string(), quantized)];
    out.extend(tensors.tensors().into_iter().filter(|(other, _)| other != name));
    serialize_to_file(out, &None, &destination.join("model.safetensors"))?;
    for file in ["tokenizer.json", "config.json"] {
        fs::copy(source.join(file), destination.join(file))?;
    }
    println!("Wrote the int8 model to {}.", destination.display());
    Ok(())
}

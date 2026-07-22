//! The notes sidecar: read a summarize request as JSON on stdin, run the local GGUF instruct model
//! via llama.cpp, and write the parsed notes as JSON on stdout. Errors go to stderr with a non-zero
//! exit — the same contract shape as `hearsay-diarize`. Loading the model per invocation (and exiting
//! after) keeps the ~GBs resident only while generating, matching the previous in-process behavior.
//!
//! This binary exists solely so llama.cpp's vendored `ggml` never links into the core alongside
//! whisper.cpp's: co-linking the two degrades the whisper refine ~5x (a `ggml` symbol collision).

use std::io::Read;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::ExitCode;

use encoding_rs::UTF_8;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use serde::{Deserialize, Serialize};

use hearsay_notes_prompt::{build_prompt, parse_notes, MeetingNotes};

/// Upper bound on the generation context (tokens). KV memory scales with `n_ctx`, so the context is
/// sized to the actual prompt + generation up to this cap; a transcript beyond it is truncated.
const N_CTX_CAP: u32 = 16_384;
/// Cap on generated tokens (a summary + action items is well under this).
const MAX_TOKENS: usize = 1024;
/// Physical decode batch (and prompt-prefill chunk) size.
const N_BATCH: usize = 512;
/// Headroom (tokens) reserved for the prompt's instruction scaffold + ChatML turns when fitting the
/// transcript to the context.
const PROMPT_OVERHEAD_TOKENS: usize = 512;

/// The request the core writes to this sidecar's stdin.
#[derive(Deserialize)]
struct Request {
    /// Absolute path to the GGUF instruct model.
    model: String,
    /// The user-editable prompt template (its `{transcript}` placeholder is filled here).
    template: String,
    /// The finalized, speaker-attributed meeting transcript.
    transcript: String,
}

/// The response this sidecar writes to stdout on success.
#[derive(Serialize)]
struct Response {
    summary: String,
    action_items: Vec<String>,
}

fn main() -> ExitCode {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("read request from stdin: {e}");
        return ExitCode::FAILURE;
    }
    let request: Request = match serde_json::from_str(&raw) {
        Ok(req) => req,
        Err(e) => {
            eprintln!("parse request JSON: {e}");
            return ExitCode::FAILURE;
        }
    };
    let notes = match summarize(
        Path::new(&request.model),
        &request.template,
        &request.transcript,
    ) {
        Ok(notes) => notes,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let response = Response {
        summary: notes.summary,
        action_items: notes.action_items,
    };
    match serde_json::to_string(&response) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("serialize response JSON: {e}");
            ExitCode::FAILURE
        }
    }
}

fn err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}

/// Truncate `transcript` so it tokenizes to at most `max_tokens`, detokenizing the kept prefix back
/// to text (a `[transcript truncated]` marker is appended when it actually cut). A char budget
/// over-counts for CJK/dense scripts, so this token-level fit is what keeps the prompt inside
/// `N_CTX_CAP`.
fn fit_transcript_to_tokens(
    llama: &LlamaModel,
    transcript: &str,
    max_tokens: usize,
) -> Result<String, String> {
    let toks = llama
        .str_to_token(transcript, AddBos::Never)
        .map_err(|e| err("tokenize transcript", e))?;
    if toks.len() <= max_tokens {
        return Ok(transcript.to_string());
    }
    let mut out = String::new();
    let mut decoder = UTF_8.new_decoder();
    for &tok in &toks[..max_tokens] {
        if let Ok(piece) = llama.token_to_piece(tok, &mut decoder, false, None) {
            out.push_str(&piece);
        }
    }
    out.push_str("\n[transcript truncated]");
    Ok(out)
}

/// Summarize `transcript` into [`MeetingNotes`] with the GGUF model at `model`, using the
/// user-editable `template`: load the model, run one instruct prompt (greedy), and parse the reply.
fn summarize(model: &Path, template: &str, transcript: &str) -> Result<MeetingNotes, String> {
    let backend = LlamaBackend::init().map_err(|e| err("llama backend init", e))?;
    let llama = LlamaModel::load_from_file(&backend, model, &LlamaModelParams::default())
        .map_err(|e| err("load notes model", e))?;

    // Fit the transcript to the context by TOKENS before building the prompt: a char budget
    // over-counts for CJK/dense scripts (~1 char/token), so a long non-Latin transcript would
    // tokenize past N_CTX_CAP and overflow the KV cache mid-prefill.
    let transcript = fit_transcript_to_tokens(
        &llama,
        transcript.trim(),
        (N_CTX_CAP as usize).saturating_sub(MAX_TOKENS + PROMPT_OVERHEAD_TOKENS),
    )?;
    let prompt = build_prompt(template, &transcript);
    let tokens = llama
        .str_to_token(&prompt, AddBos::Always)
        .map_err(|e| err("tokenize prompt", e))?;

    // Size the context to the actual need (prompt + generation), capped, so KV memory is proportional
    // to the meeting rather than a fixed worst case.
    let want = tokens.len().saturating_add(MAX_TOKENS).saturating_add(64);
    let n_ctx = (want as u32).min(N_CTX_CAP).max(N_BATCH as u32);
    let ctx_params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(n_ctx));
    let mut ctx = llama
        .new_context(&backend, ctx_params)
        .map_err(|e| err("create llama context", e))?;

    // Prefill the prompt in N_BATCH-sized chunks (a long prompt exceeds one physical batch),
    // requesting logits only for the very last prompt token.
    let mut batch = LlamaBatch::new(N_BATCH, 1);
    let last = tokens.len().saturating_sub(1);
    let mut pos: i32 = 0;
    for chunk in tokens.chunks(N_BATCH) {
        batch.clear();
        for (i, &tok) in chunk.iter().enumerate() {
            let global = pos as usize + i;
            batch
                .add(tok, pos + i as i32, &[0], global == last)
                .map_err(|e| err("prefill batch", e))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| err("prefill decode", e))?;
        pos += chunk.len() as i32;
    }

    // Greedy generation from the last prompt logits until EOS or the token cap.
    let mut out = String::new();
    let mut decoder = UTF_8.new_decoder();
    let mut sampler = LlamaSampler::greedy();
    for _ in 0..MAX_TOKENS {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if token == llama.token_eos() {
            break;
        }
        match llama.token_to_piece(token, &mut decoder, false, None) {
            Ok(piece) => out.push_str(&piece),
            Err(e) => return Err(err("detokenize", e)),
        }
        batch.clear();
        batch
            .add(token, pos, &[0], true)
            .map_err(|e| err("gen batch", e))?;
        pos += 1;
        ctx.decode(&mut batch).map_err(|e| err("gen decode", e))?;
    }

    Ok(parse_notes(&out))
}

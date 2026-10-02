//! Diarizer comparison (`make diarizer-eval`): runs each candidate engine through the
//! `hearsay-diarize --diarizer` sidecar over the AMI ES2004a near-field and far-field recordings and
//! reports speaker count, DER (missed / false alarm / confusion) and wall time. Report-only: it never
//! gates. Opt in with `HEARSAY_DIARIZER_EVAL=1`; self-skips without the audio or sidecar.
//! `HEARSAY_DIARIZER_ENGINES=a,b` restricts the engines.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use hearsay_attribution::{der, speaker_count, SpeakerTurn};
use hearsay_eval::{eval_dir, parse_rttm, resolve_audio, resolve_sidecar, write_report};
use serde::{Deserialize, Serialize};

const COLLAR_S: f64 = 0.25;
const EXPECTED_SPEAKERS: i64 = 4;
const CONDITIONS: [(&str, &str); 2] = [
    ("near-field", "ami/ES2004a.Mix-Headset.wav"),
    ("far-field", "ami/ES2004a.Array1-01.wav"),
];

struct Engine {
    label: &'static str,
    arg: &'static str,
    env: &'static [(&'static str, &'static str)],
}

const ENGINES: &[Engine] = &[
    Engine {
        label: "pyannote (shipped offline)",
        arg: "pyannote",
        env: &[],
    },
    Engine {
        label: "pyannote, 4 speakers fixed",
        arg: "pyannote",
        env: &[("HEARSAY_DIARIZE_NUM_SPEAKERS", "4")],
    },
    Engine {
        label: "pyannote, max 6 speakers",
        arg: "pyannote",
        env: &[("HEARSAY_DIARIZE_MAX_SPEAKERS", "6")],
    },
    Engine {
        label: "nemotron3 offline",
        arg: "nemotron3-offline",
        env: &[],
    },
    Engine {
        label: "nemotron3 fast32",
        arg: "nemotron3-fast32",
        env: &[],
    },
    Engine {
        label: "sortformer streaming",
        arg: "sortformer",
        env: &[],
    },
    Engine {
        label: "sortformer offline",
        arg: "sortformer-offline",
        env: &[],
    },
    Engine {
        label: "lseend ami 500ms (shipped live)",
        arg: "lseend-ami",
        env: &[],
    },
    Engine {
        label: "lseend ami 100ms",
        arg: "lseend-ami-100ms",
        env: &[],
    },
    Engine {
        label: "lseend callhome 500ms",
        arg: "lseend-callhome",
        env: &[],
    },
    Engine {
        label: "lseend dih2 500ms",
        arg: "lseend-dih2",
        env: &[],
    },
    Engine {
        label: "lseend dih3 500ms",
        arg: "lseend-dih3",
        env: &[],
    },
    Engine {
        label: "lseend dih3 100ms",
        arg: "lseend-dih3-100ms",
        env: &[],
    },
];

#[derive(Deserialize)]
struct SidecarTurn {
    speaker: String,
    start_s: f64,
    end_s: f64,
}

#[derive(Deserialize)]
struct SidecarOutput {
    duration_s: f64,
    turns: Vec<SidecarTurn>,
    #[serde(default)]
    speakers: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct Row {
    engine: String,
    arg: String,
    condition: String,
    error: Option<String>,
    speaker_count: Option<usize>,
    count_error: Option<i64>,
    der: Option<f64>,
    missed: Option<f64>,
    false_alarm: Option<f64>,
    confusion: Option<f64>,
    embeddings: Option<usize>,
    audio_s: Option<f64>,
    wall_s: f64,
    real_time_factor: Option<f64>,
}

fn run_engine(
    bin: &Path,
    engine: &Engine,
    audio: &Path,
    condition: &str,
    truth: &[SpeakerTurn],
) -> Row {
    let mut row = Row {
        engine: engine.label.to_string(),
        arg: engine.arg.to_string(),
        condition: condition.to_string(),
        error: None,
        speaker_count: None,
        count_error: None,
        der: None,
        missed: None,
        false_alarm: None,
        confusion: None,
        embeddings: None,
        audio_s: None,
        wall_s: 0.0,
        real_time_factor: None,
    };
    let mut cmd = Command::new(bin);
    cmd.arg(audio)
        .args(["--diarizer", engine.arg])
        .stderr(Stdio::null());
    for (key, value) in engine.env {
        cmd.env(key, value);
    }
    let started = Instant::now();
    let output = cmd.output();
    row.wall_s = started.elapsed().as_secs_f64();
    let output = match output {
        Ok(out) if out.status.success() => out,
        Ok(out) => {
            row.error = Some(format!("sidecar exited with {}", out.status));
            return row;
        }
        Err(e) => {
            row.error = Some(format!("spawn failed: {e}"));
            return row;
        }
    };
    let parsed: SidecarOutput = match serde_json::from_slice(&output.stdout) {
        Ok(parsed) => parsed,
        Err(e) => {
            row.error = Some(format!("bad sidecar json: {e}"));
            return row;
        }
    };
    let hyp: Vec<SpeakerTurn> = parsed
        .turns
        .iter()
        .map(|t| SpeakerTurn {
            speaker: t.speaker.clone(),
            start_s: t.start_s,
            end_s: t.end_s,
        })
        .collect();
    let breakdown = der(truth, &hyp, COLLAR_S);
    let count = speaker_count(&hyp);
    row.speaker_count = Some(count);
    row.count_error = Some(count as i64 - EXPECTED_SPEAKERS);
    row.der = Some(breakdown.der());
    row.missed = Some(breakdown.missed / breakdown.total);
    row.false_alarm = Some(breakdown.false_alarm / breakdown.total);
    row.confusion = Some(breakdown.confusion / breakdown.total);
    row.embeddings = Some(parsed.speakers.len());
    row.audio_s = Some(parsed.duration_s);
    row.real_time_factor = Some(row.wall_s / parsed.duration_s);
    row
}

fn print_table(rows: &[Row]) {
    eprintln!(
        "\n{:<34} {:<11} {:>4} {:>4} {:>7} {:>7} {:>7} {:>7} {:>5} {:>8} {:>7}",
        "engine", "condition", "spk", "err", "DER", "miss", "fa", "conf", "emb", "wall_s", "RTF"
    );
    for r in rows {
        match &r.error {
            Some(error) => eprintln!("{:<34} {:<11} FAILED: {error}", r.engine, r.condition),
            None => eprintln!(
                "{:<34} {:<11} {:>4} {:>+4} {:>7.3} {:>7.3} {:>7.3} {:>7.3} {:>5} {:>8.1} {:>7.4}",
                r.engine,
                r.condition,
                r.speaker_count.unwrap_or(0),
                r.count_error.unwrap_or(0),
                r.der.unwrap_or(f64::NAN),
                r.missed.unwrap_or(f64::NAN),
                r.false_alarm.unwrap_or(f64::NAN),
                r.confusion.unwrap_or(f64::NAN),
                r.embeddings.unwrap_or(0),
                r.wall_s,
                r.real_time_factor.unwrap_or(f64::NAN),
            ),
        }
    }
}

#[test]
fn diarizer_comparison() {
    if std::env::var("HEARSAY_DIARIZER_EVAL").as_deref() != Ok("1") {
        eprintln!("diarizer-eval: set HEARSAY_DIARIZER_EVAL=1 (make diarizer-eval); skipping");
        return;
    }
    let Some(bin) = resolve_sidecar("HEARSAY_DIARIZE_BIN", "hearsay-diarize") else {
        eprintln!("diarizer-eval: no hearsay-diarize sidecar (make swift-build); skipping");
        return;
    };
    let rttm: PathBuf = eval_dir().join("ES2004a.rttm");
    if !rttm.exists() {
        eprintln!(
            "diarizer-eval: reference RTTM absent ({}); skipping",
            rttm.display()
        );
        return;
    }
    let truth = parse_rttm(&rttm);
    let only: Option<Vec<String>> = std::env::var("HEARSAY_DIARIZER_ENGINES")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| v.split(',').map(str::to_string).collect());

    let mut rows: Vec<Row> = Vec::new();
    for (condition, rel) in CONDITIONS {
        let audio = resolve_audio(rel);
        if !audio.exists() {
            eprintln!(
                "diarizer-eval: skip {condition} (audio absent: {})",
                audio.display()
            );
            continue;
        }
        for engine in ENGINES {
            if only
                .as_ref()
                .is_some_and(|o| !o.iter().any(|a| a == engine.arg))
            {
                continue;
            }
            eprintln!("diarizer-eval: {condition} / {}", engine.label);
            rows.push(run_engine(&bin, engine, &audio, condition, &truth));
        }
    }
    if rows.is_empty() {
        eprintln!("diarizer-eval: no audio present; nothing to report");
        return;
    }
    print_table(&rows);
    let path = write_report("diarizers", &rows);
    eprintln!("diarizer-eval: report written to {}", path.display());
}

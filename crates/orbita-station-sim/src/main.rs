use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use orbita_station_sim::{
    frame_replay::run_frame_replay,
    proxy::run_proxy,
    replay::{ReplayOptions, run_replay},
    run_recorded_session, run_recorded_voice_session,
};

#[tokio::main]
async fn main() -> Result<()> {
    orbita_core::init_tracing();

    let arguments: Vec<String> = std::env::args().collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!(
            "usage: orbita-station-sim [--text TEXT | --audio FILE | --replay FILE | \
             --replay-frames FILE | --interactive | --listen ADDR --upstream URL] \
             [--url WS_URL] [--duid ID] [--platform MODEL] [--audio-dir DIR] \
             [--frames-out FILE] [--record FILE] [--timeout SECONDS] [--report FILE]"
        );
        return Ok(());
    }

    let url = value(&arguments, "--url")
        .or_else(|| std::env::var("QUASAR_URL").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:31120/uniproxy.alice.yandex.net/uni.ws".to_owned());
    let duid = value(&arguments, "--duid")
        .or_else(|| std::env::var("DUID").ok())
        .unwrap_or_else(|| "station-sim".to_owned());
    let platform = value(&arguments, "--platform")
        .or_else(|| std::env::var("PLATFORM").ok())
        .unwrap_or_else(|| "yandexmicro".to_owned());
    let audio_dir = value(&arguments, "--audio-dir").map(PathBuf::from);
    let record_path = value(&arguments, "--record").map(PathBuf::from);
    let report_path = value(&arguments, "--report").map(PathBuf::from);
    let timeout = value(&arguments, "--timeout")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(20.0);
    let timeout = Duration::from_secs_f64(timeout);

    if record_path.is_some()
        && arguments.iter().any(|argument| {
            matches!(
                argument.as_str(),
                "--replay" | "--replay-frames" | "--interactive"
            )
        })
    {
        bail!("--record supports a single --text or --audio session");
    }

    if let Some(listen) = value(&arguments, "--listen") {
        let upstream = value(&arguments, "--upstream")
            .or_else(|| std::env::var("UPSTREAM").ok())
            .context("--listen needs --upstream")?;
        run_proxy(&listen, &upstream, record_path).await?;
        return Ok(());
    }

    if let Some(frames_path) = value(&arguments, "--replay-frames") {
        let frames_out = value(&arguments, "--frames-out").map(PathBuf::from);
        let report = run_frame_replay(
            &url,
            &duid,
            &platform,
            Path::new(&frames_path),
            frames_out.as_deref(),
            timeout,
        )
        .await?;
        emit(serde_json::to_value(&report)?, report_path.as_deref())?;
        return Ok(());
    }

    if let Some(replay_path) = value(&arguments, "--replay") {
        let reports = run_replay(
            Path::new(&replay_path),
            ReplayOptions {
                url: &url,
                duid: &duid,
                platform: &platform,
                audio_dir: audio_dir.as_deref(),
                timeout,
            },
        )
        .await?;
        emit(json!({"steps": reports}), report_path.as_deref())?;
        return Ok(());
    }

    if arguments.iter().any(|argument| argument == "--interactive") {
        let stdin = BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();
        let mut reports = Vec::new();
        while let Some(line) = lines.next_line().await? {
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            let report = run_recorded_session(
                &url,
                &duid,
                &platform,
                text,
                audio_dir.as_deref(),
                timeout,
                None,
            )
            .await?;
            println!("RESULT: {}", serde_json::to_string(&report)?);
            reports.push(json!({
                "index": reports.len(),
                "kind": "text",
                "input": text,
                "report": report,
            }));
        }
        if report_path.is_some() {
            emit(json!({"steps": reports}), report_path.as_deref())?;
        }
        return Ok(());
    }

    let report = if let Some(path) =
        value(&arguments, "--audio").or_else(|| std::env::var("AUDIO").ok())
    {
        let audio = std::fs::read(&path).with_context(|| format!("reading voice file {path}"))?;
        run_recorded_voice_session(
            &url,
            &duid,
            &platform,
            &audio,
            audio_dir.as_deref(),
            timeout,
            record_path.as_deref(),
        )
        .await?
    } else if let Some(text) = value(&arguments, "--text").or_else(|| std::env::var("TEXT").ok()) {
        run_recorded_session(
            &url,
            &duid,
            &platform,
            &text,
            audio_dir.as_deref(),
            timeout,
            record_path.as_deref(),
        )
        .await?
    } else {
        bail!("one of --text, --audio, --replay or --interactive is required");
    };

    emit(serde_json::to_value(&report)?, report_path.as_deref())?;
    Ok(())
}

fn emit(value: Value, path: Option<&Path>) -> Result<()> {
    let pretty = serde_json::to_string_pretty(&value)?;
    if let Some(path) = path {
        std::fs::write(path, &pretty)
            .with_context(|| format!("writing report {}", path.display()))?;
    }
    println!("{pretty}");
    Ok(())
}

fn value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

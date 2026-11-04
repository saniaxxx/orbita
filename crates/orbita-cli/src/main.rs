//! Small operational tools for the stand: `warmup` and `latency`.
//!
//! `warmup` sends one synthetic turn to orbita-processor so the system prompt
//! prefix and llama.cpp kernels are hot after a restart. `latency` pairs the
//! processor's `process` line with quasar's first `Synthesized chunk` and prints
//! the per-turn time from the recognised text to the first audio.

use std::{process::Command, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::NaiveDateTime;
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const PROCESSOR_WS: &str = "ws://127.0.0.1:8080/process";
const ATTEMPTS: usize = 20;
const RETRY: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().collect();
    match arguments.get(1).map(String::as_str) {
        Some("warmup") => warmup().await,
        Some("ask") => {
            let text = arguments.get(2).context("ask needs a text argument")?;
            ask(text).await
        }
        Some("latency") => {
            let since = arguments.get(2).map(String::as_str).unwrap_or("-30min");
            latency(since)
        }
        _ => {
            eprintln!("usage: orbita-cli <warmup|ask TEXT|latency> [--since SPEC]");
            std::process::exit(2);
        }
    }
}

async fn warmup() -> Result<()> {
    for attempt in 1..=ATTEMPTS {
        match warmup_once().await {
            Ok(()) => {
                println!("warmup ok");
                return Ok(());
            }
            Err(error) => eprintln!("warmup attempt {attempt}: {error}"),
        }
        tokio::time::sleep(RETRY).await;
    }
    bail!("processor did not answer")
}

async fn ask(text: &str) -> Result<()> {
    println!("{}", turn(text).await?);
    Ok(())
}

async fn warmup_once() -> Result<()> {
    let _ = turn("прогрев").await?;
    Ok(())
}

async fn turn(text: &str) -> Result<String> {
    let (mut socket, _) = connect_async(PROCESSOR_WS).await.context("connect")?;
    let prepare = serde_json::json!({"type": "prepare", "data": {"directiveFunctions": {}}});
    socket
        .send(Message::Text(prepare.to_string().into()))
        .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let process = serde_json::json!({
        "type": "process",
        "data": {"metadata": {"internal": {}, "readyToUse": {}}, "text": text}
    });
    socket
        .send(Message::Text(process.to_string().into()))
        .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .context("timeout waiting for processor")?
            .context("processor closed the socket")??;
        if let Message::Text(text) = message {
            let value: serde_json::Value = serde_json::from_str(&text)?;
            if value["type"] == "partialResponse" && value["data"]["finished"] == true {
                return Ok(value["data"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned());
            }
        }
    }
}

fn latency(since: &str) -> Result<()> {
    let output = Command::new("journalctl")
        .args([
            "-u",
            "quasar-server",
            "-u",
            "orbita-processor",
            "--no-pager",
            "-o",
            "cat",
            "--since",
            since,
        ])
        .output()
        .context("run journalctl")?;
    if !output.status.success() {
        bail!(
            "journalctl failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stamp = Regex::new(r"(\d{4}-\d{2}-\d{2})[T ](\d{2}:\d{2}:\d{2}(?:\.\d+)?)")?;
    let mut current: Option<NaiveDateTime> = None;
    let mut deltas = Vec::new();

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(captures) = stamp.captures(line) else {
            continue;
        };
        let raw = format!("{} {}", &captures[1], &captures[2]);
        let Ok(moment) = NaiveDateTime::parse_from_str(&raw, "%Y-%m-%d %H:%M:%S%.f") else {
            continue;
        };
        if line.contains("process session_id=") || line.contains(" process: {") {
            current = Some(moment);
        } else if line.contains("Synthesized chunk")
            && let Some(start) = current.take()
        {
            let milliseconds = (moment - start).num_milliseconds();
            if milliseconds >= 0 {
                println!("turn: {milliseconds:>6} ms");
                deltas.push(milliseconds);
            }
        }
    }

    if deltas.is_empty() {
        println!("no completed turns in the journal");
        return Ok(());
    }
    deltas.sort_unstable();
    println!(
        "turns: {} median: {} ms min: {} ms max: {} ms",
        deltas.len(),
        deltas[deltas.len() / 2],
        deltas[0],
        deltas[deltas.len() - 1]
    );
    Ok(())
}

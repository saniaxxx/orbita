//! `orbita-glagol` — spike CLI for the local Yandex Station protocol.
//!
//! Examples:
//!   orbita-glagol discover
//!   orbita-glagol token --device-id DUID --platform yandexmicro
//!   orbita-glagol state --host 192.168.250.115 --port 1961 \
//!       --device-id DUID --platform yandexmicro
//!   orbita-glagol play-url http://192.168.250.103:8097/track.mp3 \
//!       --host 192.168.250.115 --port 1961 \
//!       --device-id DUID --platform yandexmicro --title "Track" --artist "Artist"

use std::{net::IpAddr, time::Duration};

use anyhow::{Context, Result, bail};
use orbita_glagol::{
    AudioMetadata, GlagolClient, audio_play_command, command, discover, fetch_token, play_music,
    set_volume,
};

#[derive(Debug, Default)]
struct Options {
    command: Option<String>,
    argument: Option<String>,
    host: Option<IpAddr>,
    port: Option<u16>,
    device_id: Option<String>,
    platform: Option<String>,
    quasar: Option<String>,
    format: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    watch_seconds: Option<u64>,
    timeout_seconds: Option<u64>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let options = parse_args()?;
    let Some(name) = options.command.as_deref() else {
        print_usage();
        bail!("missing command");
    };

    match name {
        "discover" => {
            let timeout = Duration::from_secs(options.timeout_seconds.unwrap_or(5));
            let stations = discover(timeout)?;
            println!("{}", serde_json::to_string_pretty(&stations)?);
        }
        "token" => {
            let device_id = required(options.device_id.as_deref(), "--device-id")?;
            let platform = options.platform.as_deref().unwrap_or("yandexmicro");
            let quasar = quasar(&options);
            let token = fetch_token(&quasar, device_id, platform).await?;
            println!("{}", serde_json::json!({"status": "ok", "token": token}));
        }
        _ => {
            let host = required(options.host, "--host")?;
            let port = required(options.port, "--port")?;
            let device_id = required(options.device_id.as_deref(), "--device-id")?;
            let platform = options.platform.as_deref().unwrap_or("yandexmicro");
            let token = fetch_token(&quasar(&options), device_id, platform).await?;
            let mut client = GlagolClient::connect(host, port, token).await?;
            if let Some(seconds) = options.timeout_seconds {
                client = client.with_timeout(Duration::from_secs(seconds));
            }

            let response = match name {
                "state" => {
                    let seconds = options.watch_seconds.unwrap_or(2);
                    let messages = client.watch(Duration::from_secs(seconds)).await?;
                    serde_json::json!({"messages": messages})
                }
                "pause" => client.send(command("stop")).await?,
                "resume" => client.send(command("play")).await?,
                "next" => client.send(command("next")).await?,
                "prev" => client.send(command("prev")).await?,
                "volume" => {
                    let volume: f32 = required(options.argument.as_deref(), "volume")?
                        .parse()
                        .context("volume must be a number")?;
                    client.send(set_volume(volume)).await?
                }
                "play-music" => {
                    let id = required(options.argument.as_deref(), "play-music id")?;
                    let media_type = options.format.as_deref().unwrap_or("Track");
                    client.send(play_music(id, media_type)).await?
                }
                "play-url" => {
                    let url = required(options.argument.as_deref(), "play-url URL")?;
                    let format = options.format.as_deref().unwrap_or("mp3");
                    let metadata = AudioMetadata {
                        id: Some(url.to_owned()),
                        title: options.title,
                        artist: options.artist,
                        image_url: None,
                    };
                    client
                        .send(audio_play_command(url, format, Some(&metadata)))
                        .await?
                }
                other => bail!("unknown command {other}"),
            };

            println!("{}", serde_json::to_string_pretty(&response)?);
            client.close().await?;
        }
    }

    Ok(())
}

fn quasar(options: &Options) -> String {
    options
        .quasar
        .clone()
        .or_else(|| std::env::var("QUASAR_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:31120".to_owned())
}

fn required<T>(value: Option<T>, name: &str) -> Result<T> {
    value.with_context(|| format!("missing {name}"))
}

fn parse_args() -> Result<Options> {
    let mut options = Options::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--host" => options.host = Some(next(&mut arguments, "--host")?.parse()?),
            "--port" => options.port = Some(next(&mut arguments, "--port")?.parse()?),
            "--device-id" => options.device_id = Some(next(&mut arguments, "--device-id")?),
            "--platform" => options.platform = Some(next(&mut arguments, "--platform")?),
            "--quasar" => options.quasar = Some(next(&mut arguments, "--quasar")?),
            "--format" => options.format = Some(next(&mut arguments, "--format")?),
            "--title" => options.title = Some(next(&mut arguments, "--title")?),
            "--artist" => options.artist = Some(next(&mut arguments, "--artist")?),
            "--watch" => {
                options.watch_seconds = Some(next(&mut arguments, "--watch")?.parse()?);
            }
            "--timeout" => {
                options.timeout_seconds = Some(next(&mut arguments, "--timeout")?.parse()?);
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other if other.starts_with("--") => bail!("unknown option {other}"),
            other => {
                if options.command.is_none() {
                    options.command = Some(other.to_owned());
                } else if options.argument.is_none() {
                    options.argument = Some(other.to_owned());
                } else {
                    bail!("unexpected argument {other}");
                }
            }
        }
    }
    Ok(options)
}

fn next(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    arguments
        .next()
        .with_context(|| format!("{option} needs a value"))
}

fn print_usage() {
    eprintln!(
        "usage:\n  orbita-glagol discover [--timeout SECONDS]\n  \
         orbita-glagol token --device-id DUID [--platform P] [--quasar URL]\n  \
         orbita-glagol <state|pause|resume|next|prev|volume|play-url|play-music> \\\n      \
         --host IP --port PORT --device-id DUID [--platform P] [--quasar URL] \\\n      \
         [--format mp3|m3u8|flac|wav] [--title T] [--artist A] [--watch SECONDS]"
    );
}

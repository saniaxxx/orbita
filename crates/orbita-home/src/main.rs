//! Diagnostic CLI for `orbita-home`.
//!
//! Examples:
//!   orbita-home --config /etc/orbita/home.json list
//!   orbita-home --config /etc/orbita/home.json get light
//!   orbita-home --config /etc/orbita/home.json set light on_off on

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use orbita_home::{Action, ActionRequest, Home, HomeConfig, Risk, SystemClock, mqtt};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mut config_path = "/etc/orbita/home.json".to_owned();
    if let Some(index) = arguments.iter().position(|arg| arg == "--config") {
        if index + 1 >= arguments.len() {
            bail!("--config needs a path");
        }
        config_path = arguments.remove(index + 1);
        arguments.remove(index);
    }
    if arguments.is_empty() || arguments[0] == "--help" || arguments[0] == "-h" {
        print_usage();
        return Ok(());
    }

    let config = Arc::new(HomeConfig::from_path(&config_path)?);
    let home = mqtt::start(Arc::clone(&config), Arc::new(SystemClock::new())).await?;

    // Give the broker a moment to deliver retained state.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    match arguments[0].as_str() {
        "list" => {
            println!("{}", serde_json::to_string_pretty(&home.list())?);
        }
        "get" => {
            let device = arguments.get(1).context("get needs a device id")?;
            println!("{}", serde_json::to_string_pretty(&home.get(device)?)?);
        }
        "set" => {
            let device = arguments.get(1).context("set needs a device id")?;
            let capability = arguments.get(2).context("set needs a capability name")?;
            let value = arguments.get(3).context("set needs a value")?;
            let action = parse_action(&home, device, capability, value)?;
            let request = ActionRequest::new(device, capability, action);
            let published = home.apply(&request, None).await?;
            println!("{}", serde_json::to_string_pretty(&published)?);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        other => bail!("unknown command {other}"),
    }

    Ok(())
}

fn parse_action(home: &Home, device_id: &str, capability: &str, value: &str) -> Result<Action> {
    let device = home
        .config()
        .devices
        .iter()
        .find(|device| device.id == device_id)
        .context(format!("unknown device {device_id}"))?;
    let capability = device
        .capability(capability)
        .context(format!("unknown capability {capability}"))?;
    if capability.risk != Risk::Safe {
        bail!(
            "{:?} action requires a policy grant; the diagnostic CLI only sends safe actions",
            capability.risk
        );
    }
    match capability.kind.as_str() {
        "bool" => match value {
            "on" | "1" | "true" => Ok(Action::OnOff { on: true }),
            "off" | "0" | "false" => Ok(Action::OnOff { on: false }),
            other => bail!("bool value must be on/off, got {other}"),
        },
        "number" => Ok(Action::Number {
            value: value.parse().context("number value")?,
        }),
        "enum" => Ok(Action::Enum {
            value: value.to_owned(),
        }),
        other => bail!("unsupported capability type {other}"),
    }
}

fn print_usage() {
    eprintln!("usage: orbita-home [--config PATH] <list|get DEVICE|set DEVICE CAP on|off|VALUE>");
}

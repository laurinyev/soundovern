//! `--silent-local`: while a client is connected, route desktop audio to a null sink so it is
//! captured but not played locally (no echo against the phone). The previous default sink is
//! restored when the client leaves or the server exits.
//!
//! Capture follows the switch automatically because the capture stream has no fixed target.

use std::process::Command;

const SINK: &str = "soundovern_silent";

#[derive(Default)]
pub struct SilentLocal {
    prev: Option<String>,
    module: Option<String>,
}

fn pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl SilentLocal {
    pub fn active(&self) -> bool {
        self.module.is_some()
    }

    pub fn engage(&mut self) {
        if self.active() {
            return;
        }
        let mut prev = pactl(&["get-default-sink"]).filter(|s| !s.is_empty());
        if prev.as_deref() == Some(SINK) {
            // Stale state from a crashed run: pick any real sink to restore to.
            prev = pactl(&["list", "short", "sinks"])
                .and_then(|l| l.lines().filter_map(|x| x.split_whitespace().nth(1)).find(|n| *n != SINK).map(String::from));
        }
        let Some(module) = pactl(&["load-module", "module-null-sink", &format!("sink_name={SINK}"), "sink_properties=device.description=Soundovern-silent"]) else {
            tracing::warn!("silent-local: could not create null sink (pactl failed)");
            return;
        };
        if pactl(&["set-default-sink", SINK]).is_none() {
            tracing::warn!("silent-local: could not set default sink");
            let _ = pactl(&["unload-module", &module]);
            return;
        }
        tracing::info!("silent-local: desktop output muted locally (was {:?})", prev);
        self.prev = prev;
        self.module = Some(module);
    }

    pub fn restore(&mut self) {
        let Some(module) = self.module.take() else { return };
        if let Some(p) = self.prev.take() {
            let _ = pactl(&["set-default-sink", &p]);
        }
        let _ = pactl(&["unload-module", &module]);
        tracing::info!("silent-local: local output restored");
    }
}

use std::mem::size_of;
use std::ptr::addr_of;
use std::env;
use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use async_std::channel::{unbounded, Receiver, Sender};
use async_std::fs::{read_dir, File};
use async_std::io::{ReadExt, WriteExt};
use async_std::net::TcpStream;
use async_std::path::PathBuf;
use async_std::prelude::StreamExt;
use async_std::task;
use clap::Parser;
use futures::future;
use input_event_codes_hashmap::EV;
use libc::input_event;

mod key;
use key::*;

use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;

/// Listen for LiveSplit hotkeys
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to LiveSplit's settings.cfg where the hotkeys will be read from
    #[arg(short, long)]
    settings: Option<String>,
    /// Name of the hotkey profile to use
    #[arg(short = 'f', long, default_value_t = String::from("Default"))]
    profile: String,
    /// Hostname or IP address where the LiveSplit server is running
    #[arg(short = 'o', long, default_value_t = String::from("localhost"))]
    host: String,
    /// Port that the LiveSplit server is listening on
    #[arg(short, long, default_value_t = 16834)]
    port: u16,
    /// Path to the keyboard device file(s) to read from
    #[arg(short, long)]
    devices: Vec<String>,
    /// Display debug information. Specify twice to show every key event.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

struct HotkeyListener {
    args: Args,
    key_state: KeyState,
}

impl HotkeyListener {
    pub fn new(args: Args) -> Result<Self> {
        let key_state = KeyState::new(args.settings.as_deref(), args.profile.as_str())?;
        Ok(Self { args, key_state })
    }

    const COMPARISONS: [&'static str; 8] = [
        "Best Segments",
        "Best Split Times",
        "Average Segments",
        "Median Segments",
        "Worst Segments",
        "Balanced PB",
        "Latest Run",
        "Personal Best",
    ];

    pub const COMPARISON_COMMANDS: [&'static [u8]; 8] = [
        b"setcomparison Best Segments\r\n",
        b"setcomparison Best Split Times\r\n",
        b"setcomparison Average Segments\r\n",
        b"setcomparison Median Segments\r\n",
        b"setcomparison Worst Segments\r\n",
        b"setcomparison Balanced PB\r\n",
        b"setcomparison Latest Run\r\n",
        b"setcomparison Personal Best\r\n",
    ];

    fn read_last_comparison(settings_path: Option<&str>) -> Result<Option<String>> {
         let mut reader = match settings_path {
            Some(s) => XmlReader::from_file(s),
            None => XmlReader::from_file(env::var("HOME")? + "/LiveSplit/settings.cfg"),
        }
        .context("Failed to open LiveSplit settings")?;
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut last_comparison = None;
        let mut found = false;

        loop {
            match reader.read_event_into(&mut buf)? {
                XmlEvent::Start(e) if e.name().as_ref() == b"LastComparison" => {
                    found = true;
                }
                XmlEvent::Text(e) if found => {
                    last_comparison = Some(e.unescape()?.to_string());
                    break;
                }
                XmlEvent::Eof => break,
                _ => (),
            }
        }
        Ok(last_comparison)
    }

    fn read_enabled_comparisons(settings_path: Option<&str>) -> Result<Vec<&'static str>> {
        let mut reader = match settings_path {
            Some(s) => XmlReader::from_file(s),
            None => XmlReader::from_file(env::var("HOME")? + "/LiveSplit/settings.cfg"),
        }
        .context("Failed to open LiveSplit settings")?;
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut in_states = false;
        let mut enabled_map: HashMap<String, bool> = HashMap::new();
        let mut current_name: Option<String> = None;

        loop {
            match reader.read_event_into(&mut buf)? {
                XmlEvent::Start(e) => {
                    if e.name().as_ref() == b"ComparisonGeneratorStates" {
                        in_states = true;
                    } else if in_states && e.name().as_ref() == b"Generator" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                current_name = Some(attr.unescape_value()?.to_string());
                            }
                        }
                    }
                }
                XmlEvent::Text(e) => {
                    if in_states {
                        if let Some(name) = current_name.take() {
                            let value = e.unescape()?.to_ascii_lowercase();
                            enabled_map.insert(name, value == "true");
                        }
                    }
                }
                XmlEvent::End(e) => {
                    if e.name().as_ref() == b"ComparisonGeneratorStates" {
                        break;
                    }
                }
                XmlEvent::Eof => break,
                _ => (),
            }
        }
        // Always include "Personal Best"
        let mut enabled: Vec<&'static str> = Self::COMPARISONS
            .iter()
            .filter(|&&c| enabled_map.get(c).copied().unwrap_or(false))
            .copied()
            .collect();
        enabled.push("Personal Best");
        Ok(enabled)
    }

    async fn listen_keyboard(sender: Sender<(u32, bool)>, path: PathBuf) -> Result<()> {
        let ev_key = EV["KEY"] as u16;
        let mut file = File::open(path).await?;
        loop {
            let (type_, code, value) = {
                let mut event_buf = [0u8; size_of::<input_event>()];
                file.read_exact(&mut event_buf).await?;
                // I don't think this is that bad because an input_event is ultimately all ints, so there are no invalid
                // bit patterns, and binrw would just be reading the exact same bytes in the exact same sequence.
                let event = unsafe { &*(addr_of!(event_buf) as *const input_event) };
                (event.type_, event.code, event.value)
            };
            // 2 = autorepeat, which we don't want to listen for
            if type_ == ev_key && value < 2 {
                let raw_code = code as u32;
                sender.send((raw_code, value != 0)).await?;
            }
        }
    }

    async fn listen_keys(mut self, receiver: Receiver<(u32, bool)>) -> Result<()> {
        let mut conn = TcpStream::connect(format!("{}:{}", self.args.host, self.args.port))
            .await
            .context("Could not connect to LiveSplit server")?;
        let mut paused = false;

        let enabled_comparisons = Self::read_enabled_comparisons(self.args.settings.as_deref())?;

        let enabled_indices: Vec<usize> = enabled_comparisons
            .iter()
            .filter_map(|&name| {
                    Self::COMPARISONS.iter().position(|&c| c == name)
            })
            .collect();

        let last_comparison = Self::read_last_comparison(self.args.settings.as_deref())?
            .unwrap_or_else(|| "Personal Best".to_string());

        let mut comparison_index = enabled_comparisons
            .iter()
            .position(|&c| c == last_comparison)
            .unwrap_or(0);
        
        let mut last_states: HashSet<(u32, bool)> = HashSet::new();

        loop {
            let (code, is_pressed) = receiver.recv().await?;
            if !last_states.insert((code, is_pressed)) {
                continue; // duplicate, skip
            }
            // Remove the opposite state to keep the set small
            last_states.remove(&(code, !is_pressed));
            if self.args.verbose > 1 {
                println!("Key {} = {}", code, is_pressed);
            }
            let active_hotkeys = self.key_state.handle_key(code, is_pressed);

            for hotkey in active_hotkeys
                .into_iter()
                .filter_map(|(hotkey, is_active)| is_active.then_some(hotkey))
            {
                if self.args.verbose > 0 {
                    println!("Sending hotkey {:?}", hotkey);
                }
                let command: &'static [u8] = match hotkey {
                    Hotkey::SplitKey => b"startorsplit\r\n",
                    Hotkey::ResetKey => b"reset\r\n",
                    Hotkey::SkipKey => b"skipsplit\r\n",
                    Hotkey::UndoKey => b"unsplit\r\n",
                    Hotkey::PauseKey => {
                        let command: &'static [u8] =
                            if paused { b"resume\r\n" } else { b"pause\r\n" };
                        paused = !paused;
                        command
                    }
                    Hotkey::SwitchComparisonNext => {
                        comparison_index = (comparison_index + 1) % enabled_indices.len();
                        Self::COMPARISON_COMMANDS[enabled_indices[comparison_index]]
                    }
                    Hotkey::SwitchComparisonPrevious => {
                        if comparison_index == 0 {
                            comparison_index = enabled_indices.len() - 1;
                        } else {
                            comparison_index -= 1;
                        }
                        Self::COMPARISON_COMMANDS[enabled_indices[comparison_index]]
                    }
                    _ => continue,
                };

                conn.write_all(command).await?;
            }
        }
    }

    pub async fn listen(self) -> Result<()> {
        // find keyboards
        let devices = if !self.args.devices.is_empty() {
            self.args.devices.iter().map(PathBuf::from).collect()
        } else {
            let mut devices = Vec::new();
            let mut entries = read_dir("/dev/input/by-path/").await?;
            while let Some(entry) = entries.next().await {
                let path = entry?.path();
                if path
                    .file_name()
                    .map_or(false, |n| n.to_string_lossy().ends_with("-event-kbd"))
                {
                    devices.push(path);
                }
            }
            devices
        };

        if devices.is_empty() {
            return Err(anyhow!("No keyboard devices found"));
        }

        if self.args.verbose > 0 {
            println!("Keyboards: {:?}", devices);
        }
        let (sender, receiver) = unbounded();
        let mut tasks: Vec<_> = devices
            .into_iter()
            .map(|d| task::spawn(Self::listen_keyboard(sender.clone(), d)))
            .collect();
        tasks.push(task::spawn(self.listen_keys(receiver)));
        future::try_join_all(tasks).await.map(|_| ())
    }
}

#[async_std::main]
async fn main() -> Result<()> {
    let listener = HotkeyListener::new(Args::parse())?;
    listener.listen().await
}

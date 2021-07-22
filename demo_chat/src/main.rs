use renet::channel::{ChannelConfig, ReliableOrderedChannelConfig};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

mod client;
mod server;

fn channels_config() -> HashMap<u8, Box<dyn ChannelConfig>> {
    let mut channels_config: HashMap<u8, Box<dyn ChannelConfig>> = HashMap::new();

    let reliable_config = ReliableOrderedChannelConfig::default();
    channels_config.insert(0, Box::new(reliable_config));
    channels_config
}

#[derive(Debug, Serialize, Deserialize)]
enum ClientMessages {
    Text(String),
    Init { nick: String },
}

#[derive(Debug, Serialize, Deserialize)]
enum ServerMessages {
    ClientConnected(String),
    ClientMessage(String, String),
    InitClient { clients: Vec<String> },
}

fn main() {
    let app = client::App::default();
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(Box::new(app), native_options);
}
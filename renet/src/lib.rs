pub mod channel;
pub mod error;
// TODO: Make private again after refactor
pub mod packet;
mod reassembly_fragment;
pub mod remote_connection;
mod sequence_buffer;
pub mod server;
mod timer;

use std::{fmt::Debug, hash::Hash};

pub trait ClientId: Copy + Debug + Hash + Eq {}
impl<T> ClientId for T where T: Copy + Debug + Hash + Eq {}
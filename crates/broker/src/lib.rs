//! Read-only brokerage integration through [SnapTrade](https://snaptrade.com): a signed API
//! client, a normalised portfolio model (accounts, balances, positions, orders, activity),
//! secure credential storage and an offline simulated portfolio.
//!
//! No trading: the client cannot place, modify or cancel orders, and the Connection Portal
//! link it generates requests read-only access.

pub mod model;
pub mod sample;
pub mod secrets;
pub mod snaptrade;

pub use model::{Account, Activity, InstrumentKind, Order, Portfolio, Position, Totals};
pub use secrets::{Credentials, SecretStore};
pub use snaptrade::SnapTrade;

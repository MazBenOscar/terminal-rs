export const VERSION = '0.1.0';

/** `scripts/bundle-macos.sh` wraps the host-architecture binary; switch to "Apple Silicon and Intel" once releases are universal. */
export const ARCH = 'Apple Silicon';
export const MIN_MACOS = '11';

export const SITE_URL = 'https://terminal-rs.vercel.app';
export const DOWNLOAD_URL =
	'https://github.com/michaelshimeles/terminal-rs/releases/latest/download/terminal-rs-universal.dmg';

export const TITLE = 'terminal-rs: a fast market terminal for the Mac';
export const DESCRIPTION =
	'A fast, keyboard-driven market terminal for the Mac, written in Rust. GPU charts, dense tiled panes and a command line for stocks, crypto, FX, Treasuries, futures, options and congressional trades.';

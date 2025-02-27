use clap::builder::styling;
use clap::{Parser, ValueEnum};

const STYLES: styling::Styles = styling::Styles::styled()
    .header(styling::AnsiColor::Green.on_default().bold())
    .usage(styling::AnsiColor::Green.on_default().bold())
    .literal(styling::AnsiColor::Blue.on_default().bold())
    .placeholder(styling::AnsiColor::Cyan.on_default());

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Copy, Clone, ValueEnum)]
pub enum CotParser {
    Deepseek,
}

#[derive(Debug, Parser)]
#[command(styles = STYLES)]
pub struct Cli {
    #[arg(short, long)]
    /// listen addr
    pub listen: String,

    #[arg(short, long)]
    /// backend addr
    pub backend: String,

    #[arg(short, long)]
    /// limit input token size
    pub input_max_token: Option<usize>,

    #[arg(long, value_enum)]
    pub cot_parser: Option<CotParser>,

    #[arg(short, long)]
    /// enable debug log
    pub debug: bool,
}

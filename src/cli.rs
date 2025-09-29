use clap::Parser;
use clap::builder::styling;

const STYLES: styling::Styles = styling::Styles::styled()
    .header(styling::AnsiColor::Green.on_default().bold())
    .usage(styling::AnsiColor::Green.on_default().bold())
    .literal(styling::AnsiColor::Blue.on_default().bold())
    .placeholder(styling::AnsiColor::Cyan.on_default());

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
    /// enable debug log
    pub debug: bool,
}

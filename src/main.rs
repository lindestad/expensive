use anyhow::Result;
use clap::Parser;

use expensive::{
    app,
    config::{self, Cli},
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = config::load(cli)?;
    app::run(config)
}

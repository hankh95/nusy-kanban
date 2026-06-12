//! CLI for nusy-safety — run hallucination tests against LLM backends.
//! EX-3435: Ollama backend removed. Only Claude API supported for safety testing.

use clap::{Parser, Subcommand};
use nusy_llm_backend::ClaudeBackend;
use nusy_safety::ZorblaxiaTest;

#[derive(Parser)]
#[command(name = "nusy-safety", about = "Safety gates for NuSy beings")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run Zorblaxia hallucination test against an LLM backend.
    Zorblaxia {
        /// LLM backend to test. Currently only "claude" is supported.
        #[arg(long, default_value = "claude")]
        backend: String,

        /// Model name (e.g., "claude-sonnet-4-20250514").
        #[arg(long)]
        model: Option<String>,

        /// Pass/fail threshold (0.0 to 1.0, default 0.8).
        #[arg(long, default_value = "0.8")]
        threshold: f64,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Zorblaxia {
            backend,
            model,
            threshold,
        } => {
            let test = ZorblaxiaTest::new().with_threshold(threshold);

            let report = match backend.as_str() {
                "claude" => {
                    let model = model.unwrap_or_else(|| "claude-sonnet-4-20250514".into());
                    let client = ClaudeBackend::new(&model)?;
                    test.run(&client).await?
                }
                other => {
                    eprintln!("Unknown backend: {other}. Only 'claude' is supported.");
                    std::process::exit(1);
                }
            };

            print!("{}", ZorblaxiaTest::format_report(&report));

            if !report.passed {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
